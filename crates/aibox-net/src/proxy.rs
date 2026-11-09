//! 代理感知（ProxyAwareness）——**只读检测，绝不修改任何系统设置**。
//!
//! - 环境变量 `HTTP_PROXY` / `HTTPS_PROXY`（大小写变体）解析；
//! - 活/死代理判定：TCP 连通 + `CONNECT` 转发成功 → 活；端口无响应/转发失败 → 死；
//! - 决策函数：死代理静默忽略、直连兜底（R1 级自愈），活代理则探测与下载经代理走；
//! - WinINET/WinHTTP 注册表读取：cfg(windows) 条件编译 + TODO（Linux CI 优先编译通过）。

use std::fmt;
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// 代理来源。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxySource {
    /// `HTTP_PROXY` / `http_proxy`
    EnvHttp,
    /// `HTTPS_PROXY` / `https_proxy`
    EnvHttps,
    /// Windows WinINET（HKCU Internet Settings，免提权只读）
    WinInet,
    /// Windows WinHTTP（注册表只读）
    WinHttp,
}

impl fmt::Display for ProxySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxySource::EnvHttp => write!(f, "env:HTTP_PROXY"),
            ProxySource::EnvHttps => write!(f, "env:HTTPS_PROXY"),
            ProxySource::WinInet => write!(f, "wininet"),
            ProxySource::WinHttp => write!(f, "winhttp"),
        }
    }
}

/// 检测到的代理配置（HTTP 代理；socks 系 ureq 需额外 feature，暂不支持）。
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub source: ProxySource,
}

/// Debug 必须脱敏：密码永不进日志/对话（凭据红线）。
impl fmt::Debug for ProxyConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProxyConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field(
                "username",
                &self
                    .username
                    .as_ref()
                    .map(|u| format!("{}***", u.chars().next().unwrap_or('*'))),
            )
            .field("password", &"***")
            .field("source", &self.source)
            .finish()
    }
}

impl ProxyConfig {
    /// 解析代理 URL：`http://user:pass@host:port`、`host:port`、`user:host` 等。
    /// socks5:// 等不支持的协议与坏值 → None（静默跳过，不报错扰民）。
    pub fn parse(raw: &str, source: ProxySource) -> Option<ProxyConfig> {
        let s = raw.trim().trim_end_matches('/');
        if s.is_empty() {
            return None;
        }
        let rest = match s.split_once("://") {
            Some(("http", r)) => r,
            Some(("socks" | "socks4" | "socks4a" | "socks5", _)) => return None, // TODO(socks): ureq 需 socks feature
            Some(_) => return None,
            None => s,
        };
        let (creds, hostpart) = match rest.rsplit_once('@') {
            Some((c, h)) => (Some(c), h),
            None => (None, rest),
        };
        let (username, password) = match creds {
            Some(c) => match c.split_once(':') {
                Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
                None => (Some(c.to_string()), None),
            },
            None => (None, None),
        };
        let (host, port) = match hostpart.rsplit_once(':') {
            Some((h, p)) => {
                let port: u16 = p.parse().ok()?;
                (h.to_string(), port)
            }
            None => (hostpart.to_string(), 80),
        };
        let host = host.trim_matches(['[', ']']).to_string();
        if host.is_empty() {
            return None;
        }
        Some(ProxyConfig {
            host,
            port,
            username,
            password,
            source,
        })
    }

    /// `host:authority` 形式（CONNECT 目标之外的展示/连接用）。
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    /// 转成 ureq 代理（凭据只进这里，不进日志）。
    pub fn to_ureq(&self) -> Result<ureq::Proxy, String> {
        let target = match (&self.username, &self.password) {
            (Some(u), Some(p)) => format!("http://{u}:{p}@{}", self.authority()),
            (Some(u), None) => format!("http://{u}@{}", self.authority()),
            _ => format!("http://{}", self.authority()),
        };
        ureq::Proxy::new(&target).map_err(|e| format!("invalid proxy: {e}"))
    }
}

/// 从环境变量 getter 读取代理（注入 getter 便于离线单测）。
/// 依次查 `HTTPS_PROXY`/`https_proxy`/`HTTP_PROXY`/`http_proxy`，按 authority 去重。
pub fn proxies_from_env(get: impl Fn(&str) -> Option<String>) -> Vec<ProxyConfig> {
    let mut out: Vec<ProxyConfig> = Vec::new();
    let keys = [
        ("HTTPS_PROXY", ProxySource::EnvHttps),
        ("https_proxy", ProxySource::EnvHttps),
        ("HTTP_PROXY", ProxySource::EnvHttp),
        ("http_proxy", ProxySource::EnvHttp),
    ];
    for (key, source) in keys {
        if let Some(raw) = get(key) {
            if let Some(proxy) = ProxyConfig::parse(&raw, source) {
                if !out.iter().any(|p| p.authority() == proxy.authority()) {
                    out.push(proxy);
                }
            }
        }
    }
    out
}

/// 读取真实环境变量（只读）。
pub fn detect_env_proxies() -> Vec<ProxyConfig> {
    proxies_from_env(|k| std::env::var(k).ok())
}

/// 代理存活判定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyLiveness {
    /// TCP 连通 + CONNECT 返回 2xx
    Alive,
    /// 失败原因（人话/日志用；绝不含凭据）
    Dead(String),
}

/// 活/死判定：TCP 连通 → `CONNECT <target>` 转发 → 2xx = 活。
///
/// - 连接失败/超时/非 2xx/EOF → 死（原因入 [`ProxyLiveness::Dead`]）。
/// - `connect_target` 形如 `example.com:443`。
pub fn check_proxy(proxy: &ProxyConfig, connect_target: &str, timeout: Duration) -> ProxyLiveness {
    let addrs = match proxy.authority().to_socket_addrs() {
        Ok(a) => a.collect::<Vec<_>>(),
        Err(e) => return ProxyLiveness::Dead(format!("resolve failed: {e}")),
    };
    let addr = match addrs.first() {
        Some(a) => *a,
        None => return ProxyLiveness::Dead("resolve returned no address".into()),
    };
    let mut stream = match TcpStream::connect_timeout(&addr, timeout) {
        Ok(s) => s,
        Err(e) => return ProxyLiveness::Dead(format!("connect failed: {e}")),
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return ProxyLiveness::Dead("set timeout failed".into());
    }
    let mut request = format!("CONNECT {connect_target} HTTP/1.1\r\nHost: {connect_target}\r\n");
    if let Some(u) = &proxy.username {
        let pass = proxy.password.clone().unwrap_or_default();
        request.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            base64_encode(&format!("{u}:{pass}"))
        ));
    }
    request.push_str("\r\n");
    if let Err(e) = stream.write_all(request.as_bytes()).and_then(|_| stream.flush()) {
        return ProxyLiveness::Dead(format!("write failed: {e}"));
    }
    let mut buf = Vec::with_capacity(512);
    let mut chunk = [0u8; 256];
    loop {
        if buf.len() > 8192 {
            return ProxyLiveness::Dead("oversized CONNECT response".into());
        }
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(2).any(|w| w == b"\r\n") {
                    break; // 状态行已到
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return ProxyLiveness::Dead("timeout waiting CONNECT response".into());
            }
            Err(e) => return ProxyLiveness::Dead(format!("read failed: {e}")),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().unwrap_or("").trim();
    match status_line.split_whitespace().nth(1) {
        Some(code) if code.starts_with('2') => ProxyLiveness::Alive,
        Some(code) => ProxyLiveness::Dead(format!("CONNECT HTTP {code}")),
        None => ProxyLiveness::Dead("empty CONNECT response".into()),
    }
}

/// 决策函数（R1 自愈）：按候选顺序取第一个**活**代理；
/// 全死 → `None`（本进程静默忽略死代理、直连）。
pub fn decide_proxy<'a>(
    candidates: &'a [ProxyConfig],
    liveness: &[ProxyLiveness],
) -> Option<&'a ProxyConfig> {
    candidates
        .iter()
        .zip(liveness.iter())
        .find(|(_, l)| matches!(l, ProxyLiveness::Alive))
        .map(|(p, _)| p)
}

/// 一站式：读环境代理 → 逐个活测 → 返回可用代理（无 → 直连）。
pub fn detect_effective_proxy(connect_target: &str, timeout: Duration) -> Option<ProxyConfig> {
    let candidates = detect_env_proxies();
    let liveness: Vec<ProxyLiveness> = candidates
        .iter()
        .map(|p| check_proxy(p, connect_target, timeout))
        .collect();
    decide_proxy(&candidates, &liveness).cloned()
}

/// Windows 注册表代理（只读，免提权）。
#[cfg(windows)]
pub fn detect_registry_proxies() -> Vec<ProxyConfig> {
    // TODO(Windows): 用 winreg 读 HKCU\Software\Microsoft\Windows\CurrentVersion\
    // Internet Settings（WinINET：ProxyEnable/ProxyServer）与 WinHTTP 的
    // HKLM SOFTWARE\Microsoft\Windows\CurrentVersion\Internet Settings\Connections
    // （或 `netsh winhttp show proxy` 输出解析）。只读、不写任何注册表键。
    // 当前返回空 = 回退 env 检测，Linux CI 不编译此分支。
    Vec::new()
}

#[cfg(not(windows))]
pub fn detect_registry_proxies() -> Vec<ProxyConfig> {
    Vec::new()
}

/// 极简 base64（标准字母表，无 padding 依赖），仅用于 CONNECT 认证头。
fn base64_encode(input: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = input.as_bytes();
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn parse_env_proxy_variants() {
        let p = ProxyConfig::parse("http://user:secret@127.0.0.1:8080", ProxySource::EnvHttp)
            .expect("parse");
        assert_eq!(p.host, "127.0.0.1");
        assert_eq!(p.port, 8080);
        assert_eq!(p.username.as_deref(), Some("user"));
        assert_eq!(p.password.as_deref(), Some("secret"));

        let p2 = ProxyConfig::parse("127.0.0.1:3128", ProxySource::EnvHttps).expect("parse");
        assert_eq!(p2.port, 3128);
        assert!(p2.username.is_none());

        let p3 = ProxyConfig::parse("proxy.local", ProxySource::EnvHttp).expect("default port");
        assert_eq!(p3.port, 80);

        // 不支持/坏值 → None
        assert!(ProxyConfig::parse("socks5://127.0.0.1:1080", ProxySource::EnvHttp).is_none());
        assert!(ProxyConfig::parse("", ProxySource::EnvHttp).is_none());
        assert!(ProxyConfig::parse("127.0.0.1:notaport", ProxySource::EnvHttp).is_none());
    }

    #[test]
    fn debug_output_never_leaks_credentials() {
        let p = ProxyConfig::parse("http://alice:hunter2@10.0.0.1:8080", ProxySource::EnvHttp)
            .unwrap();
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("hunter2"), "密码绝不能出现在 Debug: {dbg}");
        assert!(dbg.contains("***"));
        assert!(!dbg.contains("alice:") || dbg.contains("a***"));
    }

    #[test]
    fn proxies_from_env_dedup_and_order() {
        let get = |k: &str| match k {
            "HTTPS_PROXY" => Some("http://10.0.0.1:8080".into()),
            "HTTP_PROXY" => Some("http://10.0.0.1:8080".into()), // 与 HTTPS 同 authority → 去重
            "http_proxy" => Some("http://10.0.0.2:3128".into()),
            _ => None,
        };
        let list = proxies_from_env(get);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].port, 8080);
        assert_eq!(list[1].port, 3128);
        // 大小写变量：http_proxy 小写也认
        assert_eq!(list[1].source, ProxySource::EnvHttp);
    }

    #[test]
    fn base64_known_vector() {
        assert_eq!(base64_encode("user:pass"), "dXNlcjpwYXNz");
        assert_eq!(base64_encode(""), "");
        assert_eq!(base64_encode("a"), "YQ==");
        assert_eq!(base64_encode("ab"), "YWI=");
    }

    /// 起一个本地 CONNECT 代理桩：`respond` 控制回包。
    fn stub_connect_proxy(mode: &str) -> (String, Arc<AtomicBool>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hit = Arc::new(AtomicBool::new(false));
        let hit2 = Arc::clone(&hit);
        let mode = mode.to_string();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                hit2.store(true, Ordering::SeqCst);
                let mode = mode.clone();
                thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf);
                    match mode.as_str() {
                        "alive" => {
                            let _ = stream
                                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
                        }
                        "refuse_http" => {
                            let _ = stream.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n");
                        }
                        _ => {} // "hang"：不回包 → 客户端超时
                    }
                });
            }
        });
        (addr.to_string(), hit)
    }

    #[test]
    fn check_proxy_alive_via_connect_200() {
        let (addr, _hit) = stub_connect_proxy("alive");
        let p = ProxyConfig::parse(&addr, ProxySource::EnvHttp).unwrap();
        let liveness = check_proxy(&p, "example.com:443", Duration::from_secs(2));
        assert_eq!(liveness, ProxyLiveness::Alive);
    }

    #[test]
    fn check_proxy_dead_on_non_2xx_and_hang_and_refused() {
        // 非 2xx → 死
        let (addr, _h) = stub_connect_proxy("refuse_http");
        let p = ProxyConfig::parse(&addr, ProxySource::EnvHttp).unwrap();
        let l = check_proxy(&p, "example.com:443", Duration::from_secs(2));
        assert!(matches!(l, ProxyLiveness::Dead(_)), "407 → 死: {l:?}");

        // 挂起不回包 → 超时死
        let (addr, _h) = stub_connect_proxy("hang");
        let p = ProxyConfig::parse(&addr, ProxySource::EnvHttp).unwrap();
        let start = std::time::Instant::now();
        let l = check_proxy(&p, "example.com:443", Duration::from_millis(400));
        assert!(matches!(l, ProxyLiveness::Dead(_)), "超时 → 死: {l:?}");
        assert!(start.elapsed() < Duration::from_secs(3));

        // 端口无人监听 → 死
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let a = l.local_addr().unwrap();
            drop(l);
            a.to_string()
        };
        let p = ProxyConfig::parse(&dead, ProxySource::EnvHttp).unwrap();
        let l = check_proxy(&p, "example.com:443", Duration::from_millis(500));
        assert!(matches!(l, ProxyLiveness::Dead(_)), "连接拒绝 → 死: {l:?}");
    }

    #[test]
    fn decide_proxy_skips_dead_and_direct_when_all_dead() {
        let alive = ProxyConfig::parse("127.0.0.1:8080", ProxySource::EnvHttp).unwrap();
        let dead = ProxyConfig::parse("127.0.0.1:3128", ProxySource::EnvHttp).unwrap();
        let list = vec![dead.clone(), alive.clone()];

        // 前死活 → 选活的
        let chosen = decide_proxy(
            &list,
            &[
                ProxyLiveness::Dead("refused".into()),
                ProxyLiveness::Alive,
            ],
        );
        assert_eq!(chosen, Some(&alive));

        // 全死 → None（静默直连，R1 自愈）
        let chosen = decide_proxy(
            &list,
            &[
                ProxyLiveness::Dead("refused".into()),
                ProxyLiveness::Dead("timeout".into()),
            ],
        );
        assert_eq!(chosen, None);

        // 无候选 → None
        assert_eq!(decide_proxy(&[], &[]), None);
    }

    #[test]
    fn registry_probe_returns_empty_on_linux() {
        // Linux（CI）：cfg(not(windows)) 分支编译通过且返回空
        assert!(detect_registry_proxies().is_empty());
    }
}
