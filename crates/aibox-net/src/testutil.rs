//! 测试用本地 HTTP 服务器（手写、离线）：支持 HEAD / GET+Range / 故障注入 /
//! 请求日志。仅供 `cargo test -p aibox-net` 使用，不进发布产物。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicU64, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// 一条被记录的请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedRequest {
    /// 如 `GET` / `HEAD`
    pub method: String,
    /// 请求目标（可能是绝对 URI，代理场景）
    pub target: String,
    /// Range 头原文（无 → None）
    pub range: Option<String>,
}

#[derive(Debug, Default)]
struct Knobs {
    /// 丢弃前 N 个 GET 连接（不回任何字节 → 传输错误）
    drop_get: AtomicU32,
    /// 前 N 个 GET：回完整响应头（Content-Length=全长）但只写 N 字节后断开（半截体）
    truncate_get: AtomicU32,
    truncate_bytes: AtomicU64,
    /// 前 N 个 GET 返回 503
    fail_status_get: AtomicU32,
}

/// 手写 HTTP 服务器（每个连接一个线程，`Connection: close`）。
pub struct TestHttpServer {
    addr: SocketAddr,
    knobs: Arc<Knobs>,
    requests: Arc<Mutex<Vec<LoggedRequest>>>,
}

impl TestHttpServer {
    /// 起一个服务固定 body 的服务器。
    pub fn serve(body: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("local addr");
        let body = Arc::new(body);
        let knobs = Arc::new(Knobs::default());
        let requests: Arc<Mutex<Vec<LoggedRequest>>> = Arc::new(Mutex::new(Vec::new()));

        let body2 = Arc::clone(&body);
        let knobs2 = Arc::clone(&knobs);
        let requests2 = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let body = Arc::clone(&body2);
                let knobs = Arc::clone(&knobs2);
                let requests = Arc::clone(&requests2);
                thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                    handle_conn(&mut stream, &body, &knobs, &requests);
                });
            }
        });

        Self {
            addr,
            knobs,
            requests,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 后续 N 个 GET 直接断连（不回字节）。
    pub fn inject_drop_get(&self, n: u32) {
        self.knobs.drop_get.store(n, Ordering::SeqCst);
    }

    /// 后续 N 个 GET 半截体：响应头声明全长，实际只写 `bytes` 后断开。
    pub fn inject_truncate_get(&self, n: u32, bytes: u64) {
        self.knobs.truncate_get.store(n, Ordering::SeqCst);
        self.knobs.truncate_bytes.store(bytes, Ordering::SeqCst);
    }

    /// 后续 N 个 GET 返回 503。
    pub fn inject_fail_status_get(&self, n: u32) {
        self.knobs.fail_status_get.store(n, Ordering::SeqCst);
    }

    pub fn requests(&self) -> Vec<LoggedRequest> {
        self.requests.lock().expect("log lock").clone()
    }

    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("log lock").len()
    }

    /// 所有 GET 的 Range 起始偏移（无 Range → None）。
    pub fn get_range_starts(&self) -> Vec<Option<u64>> {
        self.requests()
            .iter()
            .filter(|r| r.method == "GET")
            .map(|r| r.range.as_deref().and_then(parse_range_start))
            .collect()
    }
}

fn handle_conn(
    stream: &mut std::net::TcpStream,
    body: &[u8],
    knobs: &Knobs,
    requests: &Mutex<Vec<LoggedRequest>>,
) {
    // 读请求头（到 \r\n\r\n）
    let mut req = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if req.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    if req.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(&req);
    let mut lines = text.lines();
    let request_line = lines.next().unwrap_or("").to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut range: Option<String> = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().eq_ignore_ascii_case("range") {
                range = Some(v.trim().to_string());
            }
        }
    }
    requests
        .lock()
        .expect("log lock")
        .push(LoggedRequest {
            method: method.clone(),
            target,
            range: range.clone(),
        });

    if method == "HEAD" {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.flush();
        return;
    }
    if method != "GET" {
        let _ = stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let _ = stream.flush();
        return;
    }

    // 故障注入（仅作用于 GET）
    if knobs.drop_get.load(Ordering::SeqCst) > 0 {
        knobs.drop_get.fetch_sub(1, Ordering::SeqCst);
        return; // 不回任何字节 → 客户端传输错误
    }
    if knobs.fail_status_get.load(Ordering::SeqCst) > 0 {
        knobs.fail_status_get.fetch_sub(1, Ordering::SeqCst);
        let _ = stream.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let _ = stream.flush();
        return;
    }

    if let Some(range_val) = range {
        if let Some((start, end)) = parse_range(&range_val) {
            if start >= body.len() as u64 {
                let _ = stream.write_all(
                    b"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = stream.flush();
                return;
            }
            let end = end.min(body.len() as u64 - 1);
            let slice = &body[start as usize..=end as usize];
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
                slice.len(),
                start,
                end,
                body.len()
            );
            let mut payload = head.into_bytes();
            payload.extend_from_slice(slice);
            if knobs.truncate_get.load(Ordering::SeqCst) > 0 {
                knobs.truncate_get.fetch_sub(1, Ordering::SeqCst);
                let keep = knobs.truncate_bytes.load(Ordering::SeqCst) as usize;
                // 头全发 + 只发 keep 字节体（声明全长）→ 客户端读到半截体
                let head_len = payload.len() - slice.len();
                let cut = head_len + keep.min(slice.len());
                let _ = stream.write_all(&payload[..cut]);
                let _ = stream.flush();
                return; // 断开
            }
            let _ = stream.write_all(&payload);
            let _ = stream.flush();
            return;
        }
    }

    // 无 Range / 不可解析 → 200 全量
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
        body.len()
    );
    if knobs.truncate_get.load(Ordering::SeqCst) > 0 {
        knobs.truncate_get.fetch_sub(1, Ordering::SeqCst);
        let keep = knobs.truncate_bytes.load(Ordering::SeqCst) as usize;
        let mut payload = head.into_bytes();
        payload.extend_from_slice(body);
        let head_len = payload.len() - body.len();
        let cut = head_len + keep.min(body.len());
        let _ = stream.write_all(&payload[..cut]);
        let _ = stream.flush();
        return;
    }
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();
}

/// 解析 `bytes=a-b` → (start, end)；`bytes=a-` → (a, u64::MAX)；坏值 → None。
pub fn parse_range(value: &str) -> Option<(u64, u64)> {
    let v = value.trim();
    let v = v.strip_prefix("bytes=")?;
    let (a, b) = v.split_once('-')?;
    let start: u64 = a.trim().parse().ok()?;
    let end = if b.trim().is_empty() {
        u64::MAX
    } else {
        b.trim().parse().ok()?
    };
    Some((start, end))
}

fn parse_range_start(value: &str) -> Option<u64> {
    parse_range(value).map(|(s, _)| s)
}
