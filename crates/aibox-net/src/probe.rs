//! 通道探测：HEAD + `Range: bytes=0-0`，超时 5s，记 TTFB(ms)。
//! 会话级评分 `success × 1000 / max(TTFB,1)` 与复活策略在接入。

use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub success: bool,
    pub ttfb_ms: u64,
    pub status: Option<u16>,
    pub error: Option<String>,
}

/// 对 `url` 发一次 HEAD 探测。
///
/// - 请求头带 `Range: bytes=0-0`（语义）；
/// - `timeout` 同时约束连接与总时长（基线 5s）；
/// - `ttfb_ms` = 发出请求到收到响应头的耗时（HEAD 无 body，≈TTFB）。
pub fn probe_head(url: &str, timeout: Duration) -> ProbeOutcome {
    let agent = ureq::AgentBuilder::new()
        .timeout(timeout)
        .timeout_connect(timeout)
        .build();
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
}
