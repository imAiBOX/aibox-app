//! 默认 TLS 探测实现（D5/D6）：手写 rustls 客户端，不依赖系统 C 库。
//!
//! - SNI / 证书校验 / `Host` 头全部使用 `host`（原域名），连接地址可固定到指定
//!   IP —— D5「DoH 结果 IP 直连」的关键：绕过被污染的系统 DNS，同时保持 HTTPS
//!   语义正确（SNI 必须是域名，证书必须对域名有效，否则无法区分阻断与证书问题）。
//! - 与 `aibox-net`（ureq 后端）同一 rustls 0.23 依赖图与 features（ring 后端），
//!   复用 Cargo.lock 既有条目；纯 Rust，无系统 C 库。
//! - 红线：单测全离线——本实现的**真实握手不进单测**（注入 mock `TlsProbe`），
//!   仅保留一个「连不通也必须优雅失败」的本地用例。

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::transport::{TlsAttempt, TlsProbe};

/// HTTPS 固定端口。
const HTTPS_PORT: u16 = 443;
/// 响应头读取上限（防恶意/异常服务器无限发头）。
const MAX_HEADER_BYTES: usize = 16 * 1024;

/// 生产默认 TLS 探测：rustls 手写客户端（`DoctorDeps.tls` 的默认值）。
#[derive(Debug, Default, Clone, Copy)]
pub struct RustlsHeadProbe;

/// 进程内共享的 rustls 配置（webpki 根证书 + ring provider）。
fn tls_config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let roots = RootCertStore {
                roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
            };
            // 与 ureq 相同的手法：显式指定 ring provider，避免 feature 组合歧义时
            // 进程级默认 provider 未设置导致 `builder()` panic。
            let config = ClientConfig::builder_with_provider(
                rustls::crypto::ring::default_provider().into(),
            )
            // ring provider 必然支持 TLS1.2/1.3 两套套件。
            .with_protocol_versions(&[&rustls::version::TLS12, &rustls::version::TLS13])
            .expect("ring provider supports TLS1.2 and TLS1.3")
            .with_root_certificates(roots)
            .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

fn resolve_target(host: &str, ip: Option<IpAddr>) -> Result<SocketAddr, String> {
    match ip {
        Some(ip) => Ok(SocketAddr::new(ip, HTTPS_PORT)),
        None => (host, HTTPS_PORT)
            .to_socket_addrs()
            .map_err(|e| format!("解析失败 {host}: {e}"))?
            .next()
            .ok_or_else(|| format!("解析失败 {host}: 无地址")),
    }
}

fn server_name(host: &str) -> Result<ServerName<'static>, String> {
    // rustls 不接受带方括号的 IPv6 字面量（与 ureq 同样处理）
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(bare.to_string()).map_err(|e| format!("非法 SNI 主机名 {host}: {e}"))
}

/// 找到 `\r\n\r\n`（响应头结束标志）。
fn header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// 解析 HEAD 响应：状态行 + `Date` 头（D6 时钟偏差检查用）。
fn parse_head_response(buf: &[u8]) -> TlsAttempt {
    let text = String::from_utf8_lossy(buf);
    let end = match header_end(buf) {
        Some(i) => i,
        None => {
            return if text.starts_with("HTTP/") {
                // 只收到状态行即断连：路径已通，但没有完整头（极少见）
                TlsAttempt::fail("HTTP 响应头不完整")
            } else {
                TlsAttempt::fail("未收到 HTTP 响应")
            }
        }
    };
    let head = &text[..end];
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let mut parts = status_line.split_whitespace();
    let version = parts.next().unwrap_or("");
    let status: u16 = match parts.next().and_then(|s| s.parse().ok()) {
        Some(s) if version.starts_with("HTTP/") => s,
        _ => return TlsAttempt::fail(format!("非法 HTTP 响应行: {status_line:?}")),
    };
    let date = lines.find_map(|line| {
        let (key, value) = line.split_once(':')?;
        if key.trim().eq_ignore_ascii_case("date") {
            Some(value.trim().to_string())
        } else {
            None
        }
    });
    TlsAttempt::ok(status, date)
}

impl TlsProbe for RustlsHeadProbe {
    fn head_tls(&self, host: &str, ip: Option<IpAddr>, timeout: Duration) -> TlsAttempt {
        // 先校验 SNI 主机名（不合法直接失败，不发起连接）
        let name = match server_name(host) {
            Ok(n) => n,
            Err(e) => return TlsAttempt::fail(e),
        };
        let addr = match resolve_target(host, ip) {
            Ok(a) => a,
            Err(e) => return TlsAttempt::fail(e),
        };
        let tcp = match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => s,
            Err(e) => return TlsAttempt::fail(format!("TCP 连接失败 {addr}: {e}")),
        };
        if let Err(e) = tcp
            .set_read_timeout(Some(timeout))
            .and_then(|()| tcp.set_write_timeout(Some(timeout)))
        {
            return TlsAttempt::fail(format!("设置超时失败: {e}"));
        }
        let conn = match ClientConnection::new(tls_config(), name) {
            Ok(c) => c,
            Err(e) => return TlsAttempt::fail(format!("TLS 会话创建失败: {e}")),
        };
        let mut stream = StreamOwned::new(conn, tcp);

        let req = format!(
            "HEAD / HTTP/1.1\r\nHost: {host}\r\n\
             User-Agent: aibox-netdoctor/0.0.1\r\n\
             Range: bytes=0-0\r\nConnection: close\r\n\r\n"
        );
        // 握手（含证书校验）会随读写驱动；证书类错误在这里浮出为 io::Error，
        // 其 Display 即 rustls 错误原文（如 `invalid peer certificate: Expired`）。
        if let Err(e) = stream.write_all(req.as_bytes()).and_then(|()| stream.flush()) {
            return TlsAttempt::fail(format!("TLS 写入失败: {e}"));
        }

        let mut buf = Vec::with_capacity(1024);
        let mut chunk = [0u8; 4096];
        let read_result: Result<(), String> = loop {
            if buf.len() >= MAX_HEADER_BYTES {
                break Ok(());
            }
            match stream.read(&mut chunk) {
                Ok(0) => break Ok(()),
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if header_end(&buf).is_some() {
                        break Ok(());
                    }
                }
                Err(e) => break Err(format!("TLS 读取失败: {e}")),
            }
        };
        if let Err(e) = read_result {
            return TlsAttempt::fail(e);
        }
        parse_head_response(&buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use std::time::Instant;

    /// 响应解析：完整头 / 无头 / 非法状态行。
    #[test]
    fn parse_head_response_cases() {
        let ok = b"HTTP/1.1 200 OK\r\nDate: Wed, 21 Oct 2015 07:28:00 GMT\r\nContent-Length: 0\r\n\r\n";
        let a = parse_head_response(ok);
        assert!(a.success);
        assert_eq!(a.status, Some(200));
        assert_eq!(a.date.as_deref(), Some("Wed, 21 Oct 2015 07:28:00 GMT"));

        // 无 Date 头
        let no_date = b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n";
        let b = parse_head_response(no_date);
        assert!(b.success);
        assert_eq!(b.status, Some(204));
        assert!(b.date.is_none());

        // 无响应头
        let c = parse_head_response(b"garbage");
        assert!(!c.success);
        assert!(c.error.is_some());

        // 非法状态行
        let d = parse_head_response(b"NOT-HTTP\r\n\r\n");
        assert!(!d.success);

        // 大小写不敏感的 date 键
        let e = parse_head_response(b"HTTP/1.1 200 OK\r\ndate: Tue, 01 Jan 2020 00:00:00 GMT\r\n\r\n");
        assert_eq!(e.date.as_deref(), Some("Tue, 01 Jan 2020 00:00:00 GMT"));
    }

    /// 真实握手不进单测；只验证「连不通时优雅失败 + 带原因」（离线）。
    #[test]
    fn rustls_probe_fails_gracefully_when_unreachable() {
        // 本机 443：无监听→立即拒绝；有监听→握手/证书校验也必失败（example.invalid
        // 不可能有可信证书），两条路径均优雅返回失败。
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let start = Instant::now();
        let attempt = RustlsHeadProbe.head_tls("example.invalid", Some(ip), Duration::from_secs(1));
        assert!(!attempt.success);
        assert!(attempt.error.is_some());
        assert!(attempt.status.is_none());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "拒绝连接/超时应在 timeout 内返回"
        );
    }

    /// SNI 主机名非法（空串）→ 优雅失败，不 panic。
    #[test]
    fn rustls_probe_rejects_bad_server_name() {
        let attempt = RustlsHeadProbe.head_tls("", None, Duration::from_millis(100));
        assert!(!attempt.success);
        assert!(attempt.error.is_some());
    }

    /// 联网 smoke（默认忽略，不进离线 CI）：真实握手 + Date 头解析。
    /// 手动跑：`cargo test -p aibox-netdoctor -- --ignored`
    #[test]
    #[ignore = "联网 smoke，需真实外网；离线 CI 默认跳过"]
    fn rustls_probe_real_handshake_with_date() {
        let attempt = RustlsHeadProbe.head_tls("github.com", None, Duration::from_secs(10));
        assert!(attempt.success, "真实握手应成功: {:?}", attempt.error);
        assert!(attempt.status.is_some());
        let date = attempt.date.expect("应收到 Date 头");
        let ms = crate::d6::parse_http_date_ms(&date).expect("Date 应可解析");
        let local = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let skew = crate::d6::clock_skew_ms(local, ms);
        println!("github.com date={date} skew_ms={skew}");
        // 健全性：本机时钟偏差应在 ±24h 内（超出则环境时钟本身有问题）
        assert!(skew.abs() < 86_400_000, "本机时钟偏差异常: {skew}ms");
    }
}
