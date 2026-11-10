//! 可注入探测原语（设计要求：网络探测逻辑全部可注入，单测离线 mock）。
//!
//! 默认实现复用 `aibox-net` 公开 API（`probe_head` / `DohResolver`）与 std；
//! HTTP CONNECT 隧道握手用 std `TcpStream` 手写，便于本地 TcpListener mock。

use std::net::{IpAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aibox_net::{probe_head, DohResolver, ProbeOutcome};

use crate::d1::ProxySetting;

// ---------------------------------------------------------------------------
// HTTP HEAD 探测（D0 锚测 / D4 通道实测 / D2 直连验证）
// ---------------------------------------------------------------------------

pub trait HttpProbe: Send + Sync {
    fn head(&self, url: &str, timeout: Duration) -> ProbeOutcome;
}

/// 默认实现：`aibox_net::probe_head`（HEAD + Range: bytes=0-0，记 TTFB）。
#[derive(Debug, Default, Clone, Copy)]
pub struct HeadProbe;

impl HttpProbe for HeadProbe {
    fn head(&self, url: &str, timeout: Duration) -> ProbeOutcome {
        probe_head(url, timeout)
    }
}

// ---------------------------------------------------------------------------
// TCP / 代理隧道探测（D1 死代理判定）
// ---------------------------------------------------------------------------

pub trait TcpProbe: Send + Sync {
    /// `host:port` 端口是否可建立 TCP 连接（超时视为不可达）。
    fn port_open(&self, host: &str, port: u16, timeout: Duration) -> bool;

    /// 经代理发起 HTTP `CONNECT` 隧道握手（转发探测）。
    /// Ok = 收到 2xx；Err = 连接失败 / 非 2xx / 握手异常。
    fn http_connect(
        &self,
        proxy_host: &str,
        proxy_port: u16,
        target_host: &str,
        target_port: u16,
        timeout: Duration,
    ) -> Result<(), String>;
}

/// 默认实现：std `TcpStream`（无额外依赖，Linux CI 可编译）。
#[derive(Debug, Default, Clone, Copy)]
pub struct StdTcpProbe;

impl StdTcpProbe {
    fn resolve(host: &str, port: u16) -> Option<std::net::SocketAddr> {
        (host, port).to_socket_addrs().ok()?.next()
    }
}

impl TcpProbe for StdTcpProbe {
    fn port_open(&self, host: &str, port: u16, timeout: Duration) -> bool {
        let Some(addr) = Self::resolve(host, port) else {
            return false;
        };
        TcpStream::connect_timeout(&addr, timeout).is_ok()
    }

    fn http_connect(
        &self,
        proxy_host: &str,
        proxy_port: u16,
        target_host: &str,
        target_port: u16,
        timeout: Duration,
    ) -> Result<(), String> {
        let addr = Self::resolve(proxy_host, proxy_port)
            .ok_or_else(|| format!("代理地址无法解析: {proxy_host}:{proxy_port}"))?;
        let mut stream =
            TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("连接代理失败: {e}"))?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| e.to_string())?;

        use std::io::{Read, Write};
        let req = format!(
            "CONNECT {target_host}:{target_port} HTTP/1.1\r\nHost: {target_host}:{target_port}\r\nUser-Agent: aibox-netdoctor/0.0.1\r\n\r\n"
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("发送 CONNECT 失败: {e}"))?;
        stream.flush().map_err(|e| format!("发送 CONNECT 失败: {e}"))?;

        // 逐字节读到行尾（状态行很短；避免一次性 read 不到完整行）
        let mut line = Vec::with_capacity(64);
        let mut byte = [0u8; 1];
        loop {
            if line.len() > 512 {
                return Err("CONNECT 响应异常: 状态行过长".to_string());
            }
            match stream.read(&mut byte) {
                Ok(0) => break,
                Ok(_) => {
                    line.push(byte[0]);
                    if line.ends_with(b"\r\n") {
                        break;
                    }
                }
                Err(e) => return Err(format!("读取 CONNECT 响应失败: {e}")),
            }
        }
        let text = String::from_utf8_lossy(&line);
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .ok_or_else(|| format!("CONNECT 响应异常: {text:?}"))?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(format!("CONNECT 被拒绝: HTTP {status}"))
        }
    }
}

// ---------------------------------------------------------------------------
// TLS / HTTPS 探测（D5 阻断层定位 / D6 证书异常统计）
// ---------------------------------------------------------------------------

/// 一次 HTTPS（TLS）HEAD 探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TlsAttempt {
    /// 收到合法 HTTP 响应（TLS 握手 + 证书校验均通过；任意状态码都算成功）。
    pub success: bool,
    pub status: Option<u16>,
    /// 连接 / TLS 握手 / 证书校验失败原因（可交给 `d6::classify_tls_error` 分类）。
    pub error: Option<String>,
    /// HTTP `Date` 头原文（D6 时钟偏差检查用；未收到为 None）。
    pub date: Option<String>,
}

impl TlsAttempt {
    pub fn ok(status: u16, date: Option<String>) -> Self {
        TlsAttempt {
            success: true,
            status: Some(status),
            error: None,
            date,
        }
    }

    pub fn fail(error: impl Into<String>) -> Self {
        TlsAttempt {
            success: false,
            status: None,
            error: Some(error.into()),
            date: None,
        }
    }
}

/// HTTPS 探测原语（D5/D6；单测注入 mock，生产用 `tls::RustlsHeadProbe`）。
pub trait TlsProbe: Send + Sync {
    /// 对 `host` 发一次 HTTPS HEAD：
    /// - `ip = Some(..)`：**固定连接该 IP**，但 SNI / Host / 证书校验仍用 `host`
    ///   —— D5「DoH 结果 IP 直连」的关键（绕过被污染的系统 DNS，保持 HTTPS 语义正确）；
    /// - `ip = None`：走系统解析（D6 关键域证书 / `Date` 头检查）。
    fn head_tls(&self, host: &str, ip: Option<IpAddr>, timeout: Duration) -> TlsAttempt;
}

// ---------------------------------------------------------------------------
// 域名解析（D2 三路比对）
// ---------------------------------------------------------------------------

pub trait Resolve: Send + Sync {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String>;
}

/// 系统解析（`ToSocketAddrs`，走系统 DNS 配置）。
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolve;

impl Resolve for SystemResolve {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        let addrs = (host, 443u16)
            .to_socket_addrs()
            .map_err(|e| e.to_string())?;
        let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
        if ips.is_empty() {
            Err("系统解析返回空结果".to_string())
        } else {
            Ok(ips)
        }
    }
}

/// 单端点 DoH 解析（包 `aibox_net::DohResolver`，阿里/腾讯各一路用于比对）。
#[derive(Debug)]
pub struct DohResolve {
    resolver: DohResolver,
}

impl DohResolve {
    /// 阿里 DoH（`aibox_net::DOH_ENDPOINTS[0]`，基线）。
    pub fn aliyun() -> Self {
        Self::endpoints(vec![aibox_net::DOH_ENDPOINTS[0].to_string()])
    }

    /// 腾讯 DoH（`aibox_net::DOH_ENDPOINTS[1]`，基线）。
    pub fn tencent() -> Self {
        Self::endpoints(vec![aibox_net::DOH_ENDPOINTS[1].to_string()])
    }

    /// 自定义端点（测试/调用方注入）。
    pub fn endpoints(endpoints: Vec<String>) -> Self {
        Self {
            resolver: DohResolver::with_endpoints(endpoints, Duration::from_secs(5)),
        }
    }
}

impl Resolve for DohResolve {
    fn resolve(&self, host: &str) -> Result<Vec<IpAddr>, String> {
        self.resolver.resolve(host).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// hosts 文件来源（D3；路径可注入，默认按平台）
// ---------------------------------------------------------------------------

pub trait HostsSource: Send + Sync {
    fn read(&self) -> Result<String, String>;
    fn location(&self) -> String;
}

/// 默认 hosts 路径：Linux/macOS `/etc/hosts`，Windows `System32\drivers\etc\hosts`。
pub fn default_hosts_path() -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(r"C:\Windows\System32\drivers\etc\hosts")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("/etc/hosts")
    }
}

#[derive(Debug, Clone)]
pub struct FileHosts {
    pub path: PathBuf,
}

impl Default for FileHosts {
    fn default() -> Self {
        Self {
            path: default_hosts_path(),
        }
    }
}

impl HostsSource for FileHosts {
    fn read(&self) -> Result<String, String> {
        std::fs::read_to_string(&self.path)
            .map_err(|e| format!("{}: {e}", self.path.display()))
    }

    fn location(&self) -> String {
        self.path.display().to_string()
    }
}

// ---------------------------------------------------------------------------
// 代理设置来源（D1）
// ---------------------------------------------------------------------------

pub trait ProxySource: Send + Sync {
    /// 逐项代理设置（env / WinINET / WinHTTP）。
    fn settings(&self) -> Vec<ProxySetting>;
    /// NO_PROXY 旁路配置（不是代理本身，仅入报告 detail）。
    fn no_proxy(&self) -> Option<String>;
    /// 数据来源备注（如平台限制、TODO）。
    fn notes(&self) -> Vec<String>;
}

/// 默认实现：读环境变量 +（仅 Windows）WinINET 注册表 / WinHTTP 配置。
/// **只读**，绝不写入。
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultProxySource;

const ENV_PROXY_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];

impl ProxySource for DefaultProxySource {
    fn settings(&self) -> Vec<ProxySetting> {
        let mut out = Vec::new();
        for key in ENV_PROXY_KEYS {
            // 空字符串视为未设置
            if let Some(raw) = std::env::var_os(key) {
                let raw = raw.to_string_lossy().to_string();
                if let Some((host, port)) = crate::d1::parse_proxy_endpoint(&raw) {
                    out.push(ProxySetting::env(key, &raw, &host, port));
                }
            }
        }
        #[cfg(windows)]
        {
            out.extend(wininet_settings());
            out.extend(winhttp_settings());
        }
        out
    }

    fn no_proxy(&self) -> Option<String> {
        for key in ["NO_PROXY", "no_proxy"] {
            if let Some(v) = std::env::var_os(key) {
                let v = v.to_string_lossy().to_string();
                if !v.trim().is_empty() {
                    return Some(v);
                }
            }
        }
        None
    }

    fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        #[cfg(not(windows))]
        notes.push(
            "WinINET/WinHTTP 注册表读取仅在 Windows 生效；Linux 下跳过（待 Windows 实测）"
                .to_string(),
        );
        #[cfg(windows)]
        notes.push("已读取 env + WinINET(HKCU 免提权) + WinHTTP(netsh 只读)".to_string());
        notes
    }
}

/// WinINET：`HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`
/// （免提权读取；ProxyEnable=1 才视为开启）。
#[cfg(windows)]
fn wininet_settings() -> Vec<ProxySetting> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let path = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
    let key = match hkcu.open_subkey_with_flags(path, KEY_READ) {
        Ok(k) => k,
        Err(_) => return Vec::new(),
    };
    let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
    if enabled == 0 {
        return Vec::new();
    }
    let server: String = key.get_value("ProxyServer").unwrap_or_default();
    let location = format!(r"HKCU\{path}\ProxyServer");
    crate::d1::parse_wininet_proxy_server(&server)
        .into_iter()
        .map(|(scheme, host, port)| {
            ProxySetting::new(
                crate::d1::ProxyKind::WinInet,
                location.clone(),
                &format!("{scheme}={host}:{port}"),
                &host,
                port,
            )
        })
        .collect()
}

/// WinHTTP：`netsh winhttp show proxy`（只读查询，无需管理员）。
#[cfg(windows)]
fn winhttp_settings() -> Vec<ProxySetting> {
    let out = std::process::Command::new("netsh")
        .args(["winhttp", "show", "proxy"])
        .output();
    let Ok(out) = out else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut found = Vec::new();
    for line in text.lines() {
        let is_server_line = line.contains("Proxy Server") || line.contains("代理服务器");
        if !is_server_line {
            continue;
        }
        let Some((_, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value.eq_ignore_ascii_case("null") || value == "直接访问" {
            continue;
        }
        if let Some((host, port)) = crate::d1::parse_proxy_endpoint(value) {
            found.push(ProxySetting::new(
                crate::d1::ProxyKind::WinHttp,
                "netsh winhttp show proxy".to_string(),
                value,
                &host,
                port,
            ));
        }
    }
    found
}

// ---------------------------------------------------------------------------
// 依赖汇总
// ---------------------------------------------------------------------------

/// Network Doctor 全部可注入依赖。单测逐字段替换成 mock；生产用 `Default`。
pub struct DoctorDeps {
    pub http: Box<dyn HttpProbe>,
    pub tls: Box<dyn TlsProbe>,
    pub tcp: Box<dyn TcpProbe>,
    pub system_resolve: Box<dyn Resolve>,
    pub doh_ali: Box<dyn Resolve>,
    pub doh_tencent: Box<dyn Resolve>,
    pub hosts: Box<dyn HostsSource>,
    pub proxy_source: Box<dyn ProxySource>,
    /// 探测超时基线（5s）。
    pub timeout: Duration,
    /// 时间戳来源（可注入保证测试确定性）。
    pub now_ms: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl Default for DoctorDeps {
    fn default() -> Self {
        DoctorDeps {
            http: Box::new(HeadProbe),
            tls: Box::new(crate::tls::RustlsHeadProbe),
            tcp: Box::new(StdTcpProbe),
            system_resolve: Box::new(SystemResolve),
            doh_ali: Box::new(DohResolve::aliyun()),
            doh_tencent: Box::new(DohResolve::tencent()),
            hosts: Box::new(FileHosts::default()),
            proxy_source: Box::new(DefaultProxySource),
            timeout: Duration::from_secs(5),
            now_ms: Box::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            }),
        }
    }
}
