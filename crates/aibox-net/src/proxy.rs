//! 代理感知（ProxyAwareness）——**只读检测，绝不修改任何系统设置**。
//!
//! - 环境变量 `HTTP_PROXY` / `HTTPS_PROXY`（大小写变体）解析；
//! - 活/死代理判定：TCP 连通 + HTTP/SOCKS CONNECT 转发成功 → 活；端口无响应/转发失败 → 死；
//! - 决策函数：死代理静默忽略、直连兜底（R1 级自愈），活代理则探测与下载经代理走；
//! - WinINET/WinHTTP 注册表读取：cfg(windows) 条件编译 + TODO（Linux CI 优先编译通过）。

use std::fmt;
use std::io::{Read, Write};
use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

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

/// 检测到的代理配置（HTTP / 无认证 SOCKS4、SOCKS4a、SOCKS5）。
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyConfig {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
    pub source: ProxySource,
    /// 代理协议：`http` / `socks5` / `socks4` / `socks4a`
    /// （Windows 的 `ProxyServer=socks=…` = SOCKS5；env 里常见 `socks5://…`）
    pub scheme: String,
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
            .field("scheme", &self.scheme)
            .field("source", &self.source)
            .finish()
    }
}

const UNSUPPORTED_SOCKS_AUTH: &str = "SOCKS proxy credentials are not supported";

fn is_socks_scheme(scheme: &str) -> bool {
    matches!(scheme, "socks" | "socks4" | "socks4a" | "socks5")
}

impl ProxyConfig {
    /// 解析代理 URL：`http://user:pass@host:port`、`host:port`、`user:host` 等。
    /// 不支持的协议、带凭据的 SOCKS 代理与坏值 → None（静默跳过）。
    pub fn parse(raw: &str, source: ProxySource) -> Option<ProxyConfig> {
        let s = raw.trim().trim_end_matches('/');
        if s.is_empty() {
            return None;
        }
        // 协议前缀：http / https（走 http 代理）/ socks / socks5 / socks4 / socks4a
        let (scheme, rest) = match s.split_once("://") {
            Some(("http" | "https", r)) => ("http", r),
            Some(("socks" | "socks5", r)) => ("socks5", r),
            Some(("socks4a", r)) => ("socks4a", r),
            Some(("socks4", r)) => ("socks4", r),
            Some(_) => return None,
            None => ("http", s),
        };
        let (creds, hostpart) = match rest.rsplit_once('@') {
            Some((c, h)) => (Some(c), h),
            None => (None, rest),
        };
        if is_socks_scheme(scheme) && creds.is_some() {
            return None;
        }
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
            scheme: scheme.to_string(),
        })
    }

    /// `host:authority` 形式（CONNECT 目标之外的展示/连接用）。
    pub fn authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    // ureq 的 SOCKS4/4a 始终发送空 USERID，带凭据的 SOCKS5 仍允许免认证。
    // 探测和下载统一只支持无认证 SOCKS，不让调用方误以为配置的凭据会生效。
    fn has_unsupported_socks_auth(&self) -> bool {
        is_socks_scheme(self.scheme())
            && (self.username.is_some() || self.password.is_some() || self.host.contains('@'))
    }

    /// 转成 ureq 代理（凭据不进日志）。
    pub fn to_ureq(&self) -> Result<ureq::Proxy, String> {
        if self.has_unsupported_socks_auth() {
            return Err(UNSUPPORTED_SOCKS_AUTH.into());
        }
        let scheme = self.scheme();
        if scheme != "http" && !is_socks_scheme(scheme) {
            return Err("unsupported proxy protocol".into());
        }
        let target = match (&self.username, &self.password) {
            (Some(u), Some(p)) => format!("{scheme}://{u}:{p}@{}", self.authority()),
            (Some(u), None) => format!("{scheme}://{u}@{}", self.authority()),
            _ => format!("{scheme}://{}", self.authority()),
        };
        ureq::Proxy::new(&target).map_err(|e| format!("invalid proxy: {e}"))
    }

    /// 代理协议（`http` / `socks5` / `socks4` / `socks4a`；默认 http）。
    pub fn scheme(&self) -> &str {
        if self.scheme.is_empty() {
            "http"
        } else {
            self.scheme.as_str()
        }
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
    /// TCP 连通 + HTTP CONNECT 返回 2xx 或 SOCKS CONNECT 成功
    Alive,
    /// 失败原因（人话/日志用；绝不含凭据）
    Dead(String),
}

/// 活/死判定：TCP 连通 → 按协议完成 HTTP CONNECT 或 SOCKS CONNECT。
///
/// - 连接失败/超时/转发或认证拒绝/EOF → 死（原因入 [`ProxyLiveness::Dead`]）。
/// - SOCKS TCP 握手共享截止时间；系统 DNS 解析无法通过 std 设置超时。
/// - `connect_target` 形如 `example.com:443`。
pub fn check_proxy(proxy: &ProxyConfig, connect_target: &str, timeout: Duration) -> ProxyLiveness {
    if proxy.has_unsupported_socks_auth() {
        return ProxyLiveness::Dead(UNSUPPORTED_SOCKS_AUTH.into());
    }
    let scheme = proxy.scheme();
    if scheme != "http" && !is_socks_scheme(scheme) {
        return ProxyLiveness::Dead("unsupported proxy protocol".into());
    }
    let deadline = Instant::now().checked_add(timeout);
    if timeout.is_zero() || deadline.is_none() {
        return ProxyLiveness::Dead("invalid proxy timeout".into());
    }
    let deadline = deadline.unwrap();
    let addrs = match (proxy.host.as_str(), proxy.port).to_socket_addrs() {
        Ok(a) => a.collect::<Vec<_>>(),
        Err(e) => return ProxyLiveness::Dead(format!("resolve failed: {e}")),
    };
    let addr = match addrs.first() {
        Some(a) => *a,
        None => return ProxyLiveness::Dead("resolve returned no address".into()),
    };
    let remaining = match deadline.checked_duration_since(Instant::now()) {
        Some(t) if !t.is_zero() => t,
        _ => return ProxyLiveness::Dead("proxy timeout".into()),
    };
    let mut stream = match TcpStream::connect_timeout(&addr, remaining) {
        Ok(s) => s,
        Err(e) => return ProxyLiveness::Dead(format!("connect failed: {e}")),
    };
    if stream.set_read_timeout(Some(timeout)).is_err()
        || stream.set_write_timeout(Some(timeout)).is_err()
    {
        return ProxyLiveness::Dead("set timeout failed".into());
    }
    if is_socks_scheme(scheme) {
        return match check_socks(&mut stream, proxy, connect_target, deadline) {
            Ok(()) => ProxyLiveness::Alive,
            Err(reason) => ProxyLiveness::Dead(reason.into()),
        };
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
    if let Err(e) = stream
        .write_all(request.as_bytes())
        .and_then(|_| stream.flush())
    {
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
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
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

// SOCKS 的每一次读写都使用同一个截止时间，避免对端逐字节滴流拖长探测。
// std 的系统 DNS 解析没有超时接口；代理主机与 SOCKS4 目标解析不在此保证内。
struct SocksIo<'a> {
    stream: &'a mut TcpStream,
    deadline: Instant,
}

impl SocksIo<'_> {
    fn remaining(&self) -> Result<Duration, &'static str> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|t| !t.is_zero())
            .ok_or("SOCKS handshake timeout")
    }

    fn write(&mut self, mut bytes: &[u8]) -> Result<(), &'static str> {
        while !bytes.is_empty() {
            self.stream
                .set_write_timeout(Some(self.remaining()?))
                .map_err(|_| "SOCKS set timeout failed")?;
            match self.stream.write(bytes) {
                Ok(0) => return Err("SOCKS connection closed"),
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("SOCKS write failed or timed out"),
            }
        }
        Ok(())
    }

    fn read(&mut self, mut bytes: &mut [u8]) -> Result<(), &'static str> {
        while !bytes.is_empty() {
            self.stream
                .set_read_timeout(Some(self.remaining()?))
                .map_err(|_| "SOCKS set timeout failed")?;
            match self.stream.read(bytes) {
                Ok(0) => return Err("SOCKS connection closed"),
                Ok(n) => bytes = &mut bytes[n..],
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return Err("SOCKS read failed or timed out"),
            }
        }
        Ok(())
    }
}

fn check_socks(
    stream: &mut TcpStream,
    proxy: &ProxyConfig,
    target: &str,
    deadline: Instant,
) -> Result<(), &'static str> {
    let (host, port) = target.rsplit_once(':').ok_or("invalid SOCKS target")?;
    let host = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    let port: u16 = port.parse().map_err(|_| "invalid SOCKS target port")?;
    if host.is_empty() || host.as_bytes().contains(&0) || port == 0 {
        return Err("invalid SOCKS target");
    }
    let mut io = SocksIo { stream, deadline };
    if matches!(proxy.scheme(), "socks5" | "socks") {
        io.write(&[5, 1, 0])?;
        let mut reply = [0; 2];
        io.read(&mut reply)?;
        if reply != [5, 0] {
            return Err("SOCKS5 unauthenticated method rejected");
        }
        let mut request = vec![5, 1, 0];
        match host.parse::<IpAddr>() {
            Ok(IpAddr::V4(ip)) => {
                request.push(1);
                request.extend_from_slice(&ip.octets());
            }
            Ok(IpAddr::V6(ip)) => {
                request.push(4);
                request.extend_from_slice(&ip.octets());
            }
            Err(_) => {
                if host.len() > 255 {
                    return Err("SOCKS5 target too long");
                }
                request.extend_from_slice(&[3, host.len() as u8]);
                request.extend_from_slice(host.as_bytes());
            }
        }
        request.extend_from_slice(&port.to_be_bytes());
        io.write(&request)?;
        let mut header = [0; 4];
        io.read(&mut header)?;
        if header[0] != 5 || header[1] != 0 || header[2] != 0 {
            return Err("SOCKS5 CONNECT rejected or malformed");
        }
        let address_len = match header[3] {
            1 => 4,
            4 => 16,
            3 => {
                let mut len = [0];
                io.read(&mut len)?;
                if len[0] == 0 {
                    return Err("SOCKS5 malformed bound address");
                }
                len[0] as usize
            }
            _ => return Err("SOCKS5 malformed address type"),
        };
        let mut bound = vec![0; address_len + 2];
        io.read(&mut bound)?;
    } else {
        let ip = host.parse::<IpAddr>();
        let remote_dns = proxy.scheme() == "socks4a" && ip.is_err();
        let address = match ip {
            Ok(IpAddr::V4(ip)) => ip.octets(),
            Ok(IpAddr::V6(_)) => return Err("SOCKS4 does not support IPv6"),
            Err(_) if remote_dns => [0, 0, 0, 1],
            Err(_) => (host, port)
                .to_socket_addrs()
                .map_err(|_| "SOCKS4 target resolution failed")?
                .find_map(|a| match a.ip() {
                    IpAddr::V4(ip) => Some(ip.octets()),
                    _ => None,
                })
                .ok_or("SOCKS4 target has no IPv4 address")?,
        };
        let mut request = vec![4, 1];
        request.extend_from_slice(&port.to_be_bytes());
        request.extend_from_slice(&address);
        request.push(0); // 空 USERID，与 ureq 一致。
        if remote_dns {
            request.extend_from_slice(host.as_bytes());
            request.push(0);
        }
        io.write(&request)?;
        let mut reply = [0; 8];
        io.read(&mut reply)?;
        if reply[0] != 0 || reply[1] != 90 {
            return Err("SOCKS4 CONNECT rejected or malformed");
        }
    }
    Ok(())
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

        // socks 现在**支持**了（ureq 开 socks-proxy feature；本地代理常是 socks5）
        let ps = ProxyConfig::parse("socks5://127.0.0.1:1080", ProxySource::EnvHttp)
            .expect("socks5 应被支持");
        assert_eq!(ps.scheme(), "socks5");
        // 坏值 → None
        assert!(ProxyConfig::parse("", ProxySource::EnvHttp).is_none());
        assert!(ProxyConfig::parse("127.0.0.1:notaport", ProxySource::EnvHttp).is_none());
    }

    #[test]
    fn debug_output_never_leaks_credentials() {
        let p =
            ProxyConfig::parse("http://alice:hunter2@10.0.0.1:8080", ProxySource::EnvHttp).unwrap();
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
                            let _ =
                                stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
                        }
                        "refuse_http" => {
                            let _ = stream
                                .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n");
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
            &[ProxyLiveness::Dead("refused".into()), ProxyLiveness::Alive],
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

#[cfg(test)]
mod socks_tests {
    use super::*;

    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    fn fake_socks(
        scheme: &str,
        handler: impl FnOnce(TcpStream) + Send + 'static,
    ) -> (ProxyConfig, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            handler(stream);
        });
        let config =
            ProxyConfig::parse(&format!("{scheme}://{addr}"), ProxySource::EnvHttp).unwrap();
        (config, server)
    }

    fn expect_bytes(stream: &mut TcpStream, expected: &[u8]) {
        let mut actual = vec![0; expected.len()];
        stream.read_exact(&mut actual).unwrap();
        // 不在失败诊断中打印可能包含凭据的协议字节。
        assert!(actual == expected, "unexpected protocol bytes");
    }

    fn read_http_request(stream: &mut TcpStream) -> Vec<u8> {
        let mut reader = BufReader::new(stream);
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            assert_ne!(reader.read_until(b'\n', &mut request).unwrap(), 0);
        }
        request
    }

    #[test]
    fn ureq_http_get_is_forwarded_through_no_auth_socks5() {
        const HOST: &str = "aibox-socks-test.invalid";
        const BODY: &str = "download via socks5";
        let origin = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin_addr = origin.local_addr().unwrap();
        let http = thread::spawn(move || {
            let (mut stream, _) = origin.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with(b"GET /artifact HTTP/1.1\r\n"));
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{BODY}",
                BODY.len()
            )
            .unwrap();
        });
        let (proxy, server) = fake_socks("socks5", move |mut client| {
            expect_bytes(&mut client, &[5, 1, 0]);
            client.write_all(&[5, 0]).unwrap();
            let mut connect = vec![5, 1, 0, 3, HOST.len() as u8];
            connect.extend_from_slice(HOST.as_bytes());
            connect.extend_from_slice(&origin_addr.port().to_be_bytes());
            expect_bytes(&mut client, &connect);
            // 仅代理桩知道如何把 .invalid 名称映射到本地 HTTP 源站。
            let mut upstream =
                TcpStream::connect_timeout(&origin_addr, Duration::from_secs(2)).unwrap();
            upstream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            upstream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            client.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).unwrap();
            upstream.write_all(&read_http_request(&mut client)).unwrap();
            std::io::copy(&mut upstream, &mut client).unwrap();
        });

        let agent = ureq::AgentBuilder::new()
            .try_proxy_from_env(false)
            .proxy(proxy.to_ureq().unwrap())
            .timeout(Duration::from_secs(2))
            .build();
        let response = agent
            .get(&format!("http://{HOST}:{}/artifact", origin_addr.port()))
            .call()
            .unwrap();
        assert_eq!(response.into_string().unwrap(), BODY);
        server.join().unwrap();
        http.join().unwrap();
    }

    #[test]
    fn socks5_no_auth_connect_and_complete_bound_address() {
        for reply in [
            vec![5, 0, 0, 1, 127, 0, 0, 1, 0, 80],
            vec![5, 0, 0, 3, 3, b'f', b'o', b'o', 0, 80],
            [vec![5, 0, 0, 4], vec![0; 18]].concat(),
        ] {
            let (p, server) = fake_socks("socks5", move |mut s| {
                expect_bytes(&mut s, &[5, 1, 0]);
                s.write_all(&[5, 0]).unwrap();
                expect_bytes(&mut s, b"\x05\x01\x00\x03\x0bexample.com\x01\xbb");
                s.write_all(&reply).unwrap();
            });
            assert_eq!(
                check_proxy(&p, "example.com:443", Duration::from_secs(2)),
                ProxyLiveness::Alive
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn socks5_no_auth_ip_targets() {
        for (target, address) in [
            ("127.0.0.1:443", vec![1, 127, 0, 0, 1]),
            ("[::1]:443", [vec![4], vec![0; 15], vec![1]].concat()),
        ] {
            let (p, server) = fake_socks("socks5", move |mut s| {
                expect_bytes(&mut s, &[5, 1, 0]);
                s.write_all(&[5, 0]).unwrap();
                let request = [vec![5, 1, 0], address, vec![1, 187]].concat();
                expect_bytes(&mut s, &request);
                s.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).unwrap();
            });
            assert_eq!(
                check_proxy(&p, target, Duration::from_secs(2)),
                ProxyLiveness::Alive
            );
            server.join().unwrap();
        }
    }

    #[test]
    fn socks5_rejected_method_connect_and_malformed_replies() {
        for mode in [
            "method",
            "requires_auth",
            "refused",
            "version",
            "address",
            "truncated",
        ] {
            let (p, server) = fake_socks("socks5", move |mut s| {
                expect_bytes(&mut s, &[5, 1, 0]);
                if matches!(mode, "method" | "requires_auth") {
                    s.write_all(&[5, if mode == "method" { 255 } else { 2 }])
                        .unwrap();
                    let mut byte = [0];
                    assert_eq!(s.read(&mut byte).unwrap(), 0, "must not proceed");
                    return;
                }
                s.write_all(&[5, 0]).unwrap();
                expect_bytes(&mut s, b"\x05\x01\x00\x03\x0bexample.com\x01\xbb");
                let reply: &[u8] = match mode {
                    "refused" => &[5, 5, 0, 1],
                    "version" => &[4, 0, 0, 1],
                    "address" => &[5, 0, 0, 9],
                    _ => &[5, 0, 0, 1, 127],
                };
                s.write_all(reply).unwrap();
            });
            let result = check_proxy(&p, "example.com:443", Duration::from_secs(2));
            assert!(matches!(result, ProxyLiveness::Dead(_)));
            server.join().unwrap();
        }
    }

    #[test]
    fn socks5_stall_and_trickle_share_one_deadline() {
        for trickle in [false, true] {
            let (p, server) = fake_socks("socks5", move |mut s| {
                expect_bytes(&mut s, &[5, 1, 0]);
                if trickle {
                    s.write_all(&[5, 0]).unwrap();
                    expect_bytes(&mut s, b"\x05\x01\x00\x03\x0bexample.com\x01\xbb");
                    for byte in [5, 0, 0, 1, 127, 0, 0, 1, 0, 0] {
                        if s.write_all(&[byte]).is_err() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(30));
                    }
                } else {
                    thread::sleep(Duration::from_millis(350));
                }
            });
            let start = Instant::now();
            assert!(matches!(
                check_proxy(&p, "example.com:443", Duration::from_millis(120)),
                ProxyLiveness::Dead(_)
            ));
            assert!(start.elapsed() < Duration::from_millis(300));
            server.join().unwrap();
        }
    }

    #[test]
    fn socks4_and_4a_send_empty_user_id_and_correct_address() {
        for (scheme, target, expected) in [
            (
                "socks4",
                "127.0.0.1:443",
                b"\x04\x01\x01\xbb\x7f\x00\x00\x01\x00".to_vec(),
            ),
            (
                "socks4a",
                "example.com:443",
                b"\x04\x01\x01\xbb\x00\x00\x00\x01\x00example.com\x00".to_vec(),
            ),
        ] {
            for accepted in [true, false] {
                let expected = expected.clone();
                let (p, server) = fake_socks(scheme, move |mut s| {
                    expect_bytes(&mut s, &expected);
                    s.write_all(&[0, if accepted { 90 } else { 91 }, 0, 0, 0, 0, 0, 0])
                        .unwrap();
                });
                let result = check_proxy(&p, target, Duration::from_secs(2));
                assert_eq!(matches!(result, ProxyLiveness::Alive), accepted);
                server.join().unwrap();
            }
        }
    }

    #[test]
    fn socks_urls_with_userinfo_are_rejected() {
        for scheme in ["socks", "socks4", "socks4a", "socks5"] {
            for userinfo in [
                "test-user:test-pass",
                "test-user",
                ":test-pass",
                "test-user:",
                ":",
                "",
            ] {
                let url = format!("{scheme}://{userinfo}@127.0.0.1:1080");
                assert!(ProxyConfig::parse(&url, ProxySource::EnvHttps).is_none());
            }
        }
    }

    #[test]
    fn manually_constructed_socks_with_credentials_never_connects() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        for scheme in ["socks", "socks4", "socks4a", "socks5"] {
            for (username, password, host) in [
                (Some("test-user"), Some("test-pass"), "127.0.0.1"),
                (Some(""), None, "127.0.0.1"),
                (None, Some(""), "127.0.0.1"),
                (None, None, "test-user:test-pass@127.0.0.1"),
            ] {
                let p = ProxyConfig {
                    host: host.into(),
                    port: listener.local_addr().unwrap().port(),
                    username: username.map(str::to_owned),
                    password: password.map(str::to_owned),
                    source: ProxySource::EnvHttp,
                    scheme: scheme.into(),
                };
                assert!(matches!(p.to_ureq(), Err(ref e) if e == UNSUPPORTED_SOCKS_AUTH));
                assert_eq!(
                    check_proxy(&p, "example.com:443", Duration::from_millis(100)),
                    ProxyLiveness::Dead(UNSUPPORTED_SOCKS_AUTH.into())
                );
                assert!(
                    matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
                );
            }
        }
    }

    #[test]
    fn manually_constructed_scheme_cannot_embed_socks_credentials() {
        let mut p = ProxyConfig::parse("socks5://127.0.0.1:1080", ProxySource::EnvHttp).unwrap();
        p.scheme = "socks5://test-user:test-pass@127.0.0.1:1080".into();
        assert!(matches!(p.to_ureq(), Err(ref e) if e == "unsupported proxy protocol"));
        assert_eq!(
            check_proxy(&p, "example.com:443", Duration::from_secs(2)),
            ProxyLiveness::Dead("unsupported proxy protocol".into())
        );
    }

    #[test]
    fn socks5_invalid_target_fails_without_protocol_bytes() {
        for target in ["missing-port", "example.com:0"] {
            let (p, server) = fake_socks("socks5", |mut s| {
                let mut byte = [0];
                assert_eq!(s.read(&mut byte).unwrap(), 0);
            });
            assert!(matches!(
                check_proxy(&p, target, Duration::from_secs(2)),
                ProxyLiveness::Dead(_)
            ));
            server.join().unwrap();
        }
    }

    /// SOCKS 代理：以前直接丢弃（ureq 没开 socks feature）→ 现在支持 socks4/4a/5。
    #[test]
    fn socks_urls_are_parsed_and_keep_scheme() {
        let c = ProxyConfig::parse("socks5://127.0.0.1:1080", ProxySource::EnvHttps).unwrap();
        assert_eq!(c.scheme(), "socks5");
        assert_eq!(c.host, "127.0.0.1");
        assert_eq!(c.port, 1080);

        for (prefix, scheme) in [
            ("socks4", "socks4"),
            ("socks4a", "socks4a"),
            ("socks", "socks5"),
        ] {
            let c =
                ProxyConfig::parse(&format!("{prefix}://127.0.0.1:1080"), ProxySource::EnvHttps)
                    .unwrap();
            assert_eq!(c.scheme(), scheme);
            assert!(c.to_ureq().is_ok());
        }
    }

    #[test]
    fn http_default_scheme_unchanged() {
        let c = ProxyConfig::parse("http://127.0.0.1:7890", ProxySource::EnvHttp).unwrap();
        assert_eq!(c.scheme(), "http");
        // to_ureq 必须能构造出代理（ureq 已开 socks-proxy feature）
        assert!(c.to_ureq().is_ok());
        let s = ProxyConfig::parse("socks5://127.0.0.1:1080", ProxySource::EnvHttp).unwrap();
        assert!(s.to_ureq().is_ok());
    }
}
