//! 通道探测：HEAD + `Range: bytes=0-0`，超时 5s，记 TTFB(ms)。
//! 会话级评分与复活策略见 [`crate::ProbeScore`]。

use std::time::{Duration, Instant};

use crate::proxy::ProxyConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub success: bool,
    pub ttfb_ms: u64,
    pub status: Option<u16>,
    pub error: Option<String>,
}

/// 对 `url` 发一次 HEAD 探测（直连）。
pub fn probe_head(url: &str, timeout: Duration) -> ProbeOutcome {
    probe_head_with(url, timeout, None)
}

/// 对 `url` 发一次 HEAD 探测，可经代理（活代理则探测经代理走）。
///
/// - 请求头带 `Range: bytes=0-0`（语义）；
/// - `timeout` 同时约束连接与总时长（基线 5s）；
/// - `ttfb_ms` = 发出请求到收到响应头的耗时（HEAD 无 body，≈TTFB）；
/// - 代理配置非法 → 静默回退直连（R1 自愈，不阻断探测）。
pub fn probe_head_with(
    url: &str,
    timeout: Duration,
    proxy: Option<&ProxyConfig>,
) -> ProbeOutcome {
    let mut builder = ureq::AgentBuilder::new()
        .timeout(timeout)
        .timeout_connect(timeout)
        .max_idle_connections(0)
        .user_agent("aibox-net/0.0.1");
    if let Some(p) = proxy {
        if let Ok(ureq_proxy) = p.to_ureq() {
            builder = builder.proxy(ureq_proxy);
        }
    }
    let agent = builder.build();
    let start = Instant::now();
    let result = agent
        .head(url)
        .set("Range", "bytes=0-0")
        .set("User-Agent", "aibox-net/0.0.1")
        .set("Connection", "close")
        .call();
    let ttfb_ms = start.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
    match result {
        Ok(resp) => ProbeOutcome {
            success: true,
            ttfb_ms,
            status: Some(resp.status()),
            error: None,
        },
        Err(ureq::Error::Status(code, _)) => ProbeOutcome {
            success: false,
            ttfb_ms,
            status: Some(code),
            error: Some(format!("HTTP {code}")),
        },
        Err(e) => ProbeOutcome {
            success: false,
            ttfb_ms,
            status: None,
            error: Some(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::{ProxyConfig, ProxySource};
    use crate::testutil::TestHttpServer;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    const OK_RESPONSE: &str = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    /// 手写一次性 HTTP 服务器：accept → 读请求 → 延迟 → 回 200。返回可探测的 URL。
    fn serve_once(delay_ms: u64) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local server");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                // 延迟回包：保证 TTFB(ms) 严格 > 0（loopback 无延迟时可能测得 0ms）
                thread::sleep(Duration::from_millis(delay_ms));
                let _ = stream.write_all(OK_RESPONSE.as_bytes());
                let _ = stream.flush();
            }
        });
        format!("http://{addr}/")
    }

    /// 通道 HEAD 探测（离线可过，不访问外网）。
    #[test]
    fn probe_head_local_http_server() {
        // 场景 A：本地服务器返回 200 → 探测成功且 TTFB > 0
        let url = serve_once(60);
        let outcome = probe_head(&url, Duration::from_secs(5));
        assert!(outcome.success, "probe should succeed: {outcome:?}");
        assert_eq!(outcome.status, Some(200));
        assert!(
            outcome.ttfb_ms > 0,
            "TTFB should be > 0ms: {outcome:?}"
        );

        // 场景 B：连接拒绝（端口已释放）→ 探测失败且带错误原因
        let dead_url = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            let addr = listener.local_addr().expect("local addr");
            drop(listener); // 立即释放 → 后续连接被拒绝
            format!("http://{addr}/")
        };
        let refused = probe_head(&dead_url, Duration::from_secs(5));
        assert!(!refused.success, "refused connection must fail: {refused:?}");
        assert!(refused.error.is_some(), "failure must carry a reason");
        assert!(refused.status.is_none(), "no HTTP status on connect failure");
    }

    /// 活代理探测：请求确实发给代理（absolute-form），探测成功。
    #[test]
    fn probe_head_routes_through_proxy() {
        // 本地服务器充当 HTTP 代理：任意 HEAD（绝对 URI）→ 200
        let proxy_srv = TestHttpServer::serve(Vec::new());
        let addr = proxy_srv.base_url().trim_start_matches("http://").to_string();
        let proxy = ProxyConfig::parse(&addr, ProxySource::EnvHttp).expect("parse proxy");

        // 目标是必然解析不了的域名：只有走代理才可能成功
        let outcome = probe_head_with(
            "http://nonexistent.invalid/f.bin",
            Duration::from_secs(3),
            Some(&proxy),
        );
        assert!(outcome.success, "经代理探测应成功: {outcome:?}");
        assert_eq!(outcome.status, Some(200));

        let reqs = proxy_srv.requests();
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].method, "HEAD");
        assert!(
            reqs[0].target.starts_with("http://nonexistent.invalid/"),
            "代理收到的必须是绝对 URI: {:?}",
            reqs[0].target
        );
    }

    /// 死代理（连接拒绝）→ 探测失败且带原因（上层据此静默直连）。
    #[test]
    fn probe_head_with_dead_proxy_fails_cleanly() {
        let dead = {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let a = l.local_addr().unwrap();
            drop(l);
            a.to_string()
        };
        let proxy = ProxyConfig::parse(&dead, ProxySource::EnvHttp).unwrap();
        let outcome = probe_head_with(
            "http://example.invalid/",
            Duration::from_millis(500),
            Some(&proxy),
        );
        assert!(!outcome.success, "{outcome:?}");
        assert!(outcome.error.is_some());
    }
}
