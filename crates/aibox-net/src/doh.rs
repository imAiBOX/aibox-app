//! DoH 解析器：进程内域名解析，绕过污染 DNS，不改系统设置。
//!
//! - **并发池竞速**：阿里 223.5.5.5 / 腾讯 1.12.12.12 / Google dns.google 同时查询，
//!   首个成功者胜（threads + channel，无 tokio）。
//! - **TTL 缓存**：300s（时钟注入，可离线测过期语义）。
//! - **触发语义**：CN 序默认启用，或系统解析失败/超时时启用 → [`doh_should_enable`]。
//! - **网络 IO 与解析分离**：[`parse_doh_response`] 纯函数；[`DohResolver`] 的
//!   网络出口可注入（[`DohFetcher`]），单测用固定 JSON + 手动时钟，不真联网。

use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::clock::{system_clock, SharedClock};
use crate::region::RegionKind;

/// DoH 端点基线（并发池顺序 = CN 序优先；GLOBAL 场景 Google 兜底）。
pub const DOH_ENDPOINTS: [&str; 3] = [
    "https://223.5.5.5/dns-query",   // 阿里
    "https://1.12.12.12/dns-query",  // 腾讯
    "https://dns.google/dns-query",  // Google
];

/// 解析缓存 TTL：300s。
pub const DOH_TTL_SECS: u64 = 300;

/// 可注入网络出口（测试喂固定 JSON）。
pub type DohFetcher = Arc<dyn Fn(&str) -> Result<String, DohError> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DohError {
    Transport(String),
    Http(u16),
    Parse(String),
    NoAnswer(String),
}

impl fmt::Display for DohError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DohError::Transport(e) => write!(f, "DoH transport error: {e}"),
            DohError::Http(code) => write!(f, "DoH HTTP status {code}"),
            DohError::Parse(e) => write!(f, "DoH response parse error: {e}"),
            DohError::NoAnswer(ctx) => write!(f, "DoH no usable answer: {ctx}"),
        }
    }
}

impl std::error::Error for DohError {}

/// `application/dns-json` 响应中的 Answer 条目。
#[derive(Debug, serde::Deserialize)]
struct DohAnswer {
    #[serde(rename = "type")]
    rtype: u16,
    data: String,
}

#[derive(Debug, serde::Deserialize)]
struct DohMessage {
    #[serde(rename = "Answer", default)]
    answer: Option<Vec<DohAnswer>>,
}

/// 纯函数：DNS JSON 响应 → IP 列表（A=IPv4、AAAA=IPv6，保持应答顺序，跳过 CNAME 等非 IP 条目）。
///
/// 不做任何网络 IO —— 单测直接喂固定 JSON。
pub fn parse_doh_response(json: &str) -> Result<Vec<IpAddr>, DohError> {
    let msg: DohMessage =
        serde_json::from_str(json).map_err(|e| DohError::Parse(e.to_string()))?;
    let mut ips = Vec::new();
    for ans in msg.answer.into_iter().flatten() {
        match ans.rtype {
            1 => {
                if let Ok(ip) = ans.data.parse::<Ipv4Addr>() {
                    ips.push(IpAddr::V4(ip));
                }
            }
            28 => {
                if let Ok(ip) = ans.data.parse::<Ipv6Addr>() {
                    ips.push(IpAddr::V6(ip));
                }
            }
            _ => {} // CNAME 等：跳过
        }
    }
    Ok(ips)
}

/// 触发语义（纯函数）：CN 序默认启用 DoH；GLOBAL 序仅在系统解析失败/超时时启用。
pub fn doh_should_enable(region: RegionKind, system_resolve_failed: bool) -> bool {
    region == RegionKind::Cn || system_resolve_failed
}

/// 默认网络出口：ureq GET（rustls，无系统 C 库）。
fn default_fetcher() -> DohFetcher {
    Arc::new(|url: &str| -> Result<String, DohError> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .timeout_connect(Duration::from_secs(5))
            .max_idle_connections(0)
            .user_agent("aibox-net/0.0.1")
            .build();
        let result = agent
            .get(url)
            .set("Accept", "application/dns-json")
            .call();
        match result {
            Ok(resp) => resp
                .into_string()
                .map_err(|e| DohError::Transport(e.to_string())),
            Err(ureq::Error::Status(code, _)) => Err(DohError::Http(code)),
            Err(ureq::Error::Transport(t)) => Err(DohError::Transport(t.to_string())),
        }
    })
}

#[derive(Debug)]
struct CacheEntry {
    ips: Vec<IpAddr>,
    at_ms: u64,
}

/// 进程内 DoH 解析器：并发池竞速 + TTL 缓存 + 可注入时钟/网络出口。
/// （fetcher 是闭包，不实现 Debug → 手写。）
pub struct DohResolver {
    endpoints: Vec<String>,
    timeout: Duration,
    fetcher: DohFetcher,
    cache: Mutex<HashMap<(String, u16), CacheEntry>>,
    clock: SharedClock,
    ttl_ms: u64,
}

impl std::fmt::Debug for DohResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DohResolver")
            .field("endpoints", &self.endpoints)
            .field("timeout", &self.timeout)
            .field("fetcher", &"<dyn Fn>")
            .field(
                "cache_len",
                &self.cache.lock().map(|c| c.len()).unwrap_or(0),
            )
            .field("ttl_ms", &self.ttl_ms)
            .finish_non_exhaustive()
    }
}

impl Default for DohResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl DohResolver {
    /// 默认构造：基线三端点、5s 超时、ureq 出口、真实时钟、TTL 300s。
    pub fn new() -> Self {
        Self::with_parts(
            DOH_ENDPOINTS.iter().map(|s| (*s).to_string()).collect(),
            Duration::from_secs(5),
            default_fetcher(),
            system_clock(),
            DOH_TTL_SECS * 1000,
        )
    }

    /// 全参数构造（测试注入 fetcher / clock / ttl）。
    pub fn with_parts(
        endpoints: Vec<String>,
        timeout: Duration,
        fetcher: DohFetcher,
        clock: SharedClock,
        ttl_ms: u64,
    ) -> Self {
        Self {
            endpoints,
            timeout,
            fetcher,
            cache: Mutex::new(HashMap::new()),
            clock,
            ttl_ms,
        }
    }

    /// 兼容构造：自定义端点顺序 + 真实出口/时钟（竞速语义同 `new`）。
    pub fn with_endpoints(endpoints: Vec<String>, timeout: Duration) -> Self {
        Self::with_parts(endpoints, timeout, default_fetcher(), system_clock(), DOH_TTL_SECS * 1000)
    }

    /// 解析域名（默认查询 A 记录）。
    pub fn resolve(&self, name: &str) -> Result<Vec<IpAddr>, DohError> {
        self.resolve_type(name, 1)
    }

    /// 并发池竞速解析：全部端点同时查询，首个成功（非空答案）者胜；
    /// 未命中缓存时才发网络请求；结果写入 TTL 缓存。
    pub fn resolve_type(&self, name: &str, qtype: u16) -> Result<Vec<IpAddr>, DohError> {
        let key = (normalize_name(name), qtype);
        let now = self.clock.now_ms();
        if let Some(entry) = self.cache.lock().expect("doh cache lock").get(&key) {
            if now.saturating_sub(entry.at_ms) < self.ttl_ms {
                return Ok(entry.ips.clone());
            }
        }
        let ips = self.race(name, qtype)?;
        self.cache
            .lock()
            .expect("doh cache lock")
            .insert(key, CacheEntry {
                ips: ips.clone(),
                at_ms: self.clock.now_ms(),
            });
        Ok(ips)
    }

    /// 竞速实现：每个端点一个线程，结果汇入 channel，按到达顺序取首个成功。
    fn race(&self, name: &str, qtype: u16) -> Result<Vec<IpAddr>, DohError> {
        let (tx, rx) = mpsc::channel();
        let mut spawned = 0usize;
        for endpoint in &self.endpoints {
            let url = format!("{endpoint}?name={name}&type={qtype}");
            let fetcher = Arc::clone(&self.fetcher);
            let tx = tx.clone();
            thread::spawn(move || {
                let result = fetcher(&url).and_then(|body| {
                    let ips = parse_doh_response(&body)?;
                    if ips.is_empty() {
                        Err(DohError::NoAnswer(url))
                    } else {
                        Ok(ips)
                    }
                });
                let _ = tx.send(result);
            });
            spawned += 1;
        }
        drop(tx);

        let wait = self.timeout + Duration::from_secs(2);
        let mut last_err: Option<DohError> = None;
        for _ in 0..spawned {
            match rx.recv_timeout(wait) {
                Ok(Ok(ips)) => return Ok(ips), // 首成者胜
                Ok(Err(e)) => last_err = Some(e),
                Err(_) => break, // 超时或全部发送端断开
            }
        }
        Err(last_err.unwrap_or_else(|| DohError::NoAnswer("no DoH endpoint configured".into())))
    }

    /// 直接读缓存（不发网络请求）；过期或未缓存 → None。
    pub fn cached(&self, name: &str, qtype: u16) -> Option<Vec<IpAddr>> {
        let key = (normalize_name(name), qtype);
        let now = self.clock.now_ms();
        self.cache
            .lock()
            .expect("doh cache lock")
            .get(&key)
            .filter(|e| now.saturating_sub(e.at_ms) < self.ttl_ms)
            .map(|e| e.ips.clone())
    }

    /// 清空缓存（诊断/设置变更时用）。
    pub fn clear_cache(&self) {
        self.cache.lock().expect("doh cache lock").clear();
    }

    pub fn cache_len(&self) -> usize {
        self.cache.lock().expect("doh cache lock").len()
    }

    pub fn endpoints(&self) -> &[String] {
        &self.endpoints
    }
}

fn normalize_name(name: &str) -> String {
    name.trim().trim_end_matches('.').to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const SAMPLE_JSON: &str = r#"{
        "Status": 0,
        "Answer": [
            { "name": "github.com.", "type": 5, "TTL": 59, "data": "github.com." },
            { "name": "github.com.", "type": 1, "TTL": 59, "data": "140.82.113.3" },
            { "name": "github.com.", "type": 1, "TTL": 59, "data": "140.82.114.3" },
            { "name": "github.com.", "type": 28, "TTL": 59, "data": "2606:50c0:8000::153" }
        ]
    }"#;

    fn ips_of(json: &str) -> Vec<IpAddr> {
        parse_doh_response(json).unwrap()
    }

    /// DoH「响应 JSON → IP 列表」纯函数解析（不联网）。
    #[test]
    fn parse_doh_response_returns_ip_list() {
        let ips = ips_of(SAMPLE_JSON);
        let expected: Vec<IpAddr> = ["140.82.113.3", "140.82.114.3", "2606:50c0:8000::153"]
            .iter()
            .map(|s| s.parse::<IpAddr>().unwrap())
            .collect();
        assert_eq!(ips, expected, "CNAME skipped; A/AAAA kept in order");

        // NXDOMAIN（无 Answer 字段）→ 空列表而非报错
        let nxdomain = r#"{"Status":3,"Question":[{"name":"no.aibox.im.","type":1}]}"#;
        assert!(parse_doh_response(nxdomain).unwrap().is_empty());

        // 非法 JSON → 明确报错
        assert!(parse_doh_response("{not json").is_err());
    }

    #[test]
    fn doh_trigger_semantics() {
        assert!(doh_should_enable(RegionKind::Cn, false), "CN 序默认启用");
        assert!(doh_should_enable(RegionKind::Global, true), "系统解析失败即 GLOBAL 也启用");
        assert!(!doh_should_enable(RegionKind::Global, false), "GLOBAL 且系统正常 → 不启用");
    }

    /// 固定 JSON 出口 + 调用计数（不真联网）；`(前缀, 延迟ms, 结果)`。
    fn fake_fetcher(
        responses: Vec<(&'static str, u64, Result<String, DohError>)>,
        counter: Arc<AtomicUsize>,
    ) -> DohFetcher {
        Arc::new(move |url: &str| {
            counter.fetch_add(1, Ordering::SeqCst);
            for (prefix, delay, resp) in &responses {
                if url.starts_with(prefix) {
                    if *delay > 0 {
                        thread::sleep(Duration::from_millis(*delay));
                    }
                    return resp.clone();
                }
            }
            Err(DohError::Transport(format!("no fake response for {url}")))
        })
    }

    /// 等待计数器到达目标值（竞速线程异步完成）。
    fn wait_count(counter: &AtomicUsize, target: usize) -> usize {
        for _ in 0..200 {
            let c = counter.load(Ordering::SeqCst);
            if c >= target {
                return c;
            }
            thread::sleep(Duration::from_millis(10));
        }
        counter.load(Ordering::SeqCst)
    }

    fn endpoints() -> Vec<String> {
        vec![
            "https://ali.test/dns-query".into(),
            "https://tencent.test/dns-query".into(),
            "https://google.test/dns-query".into(),
        ]
    }

    fn resolver_with(fetcher: DohFetcher, clock: SharedClock) -> DohResolver {
        DohResolver::with_parts(
            endpoints(),
            Duration::from_secs(2),
            fetcher,
            clock,
            DOH_TTL_SECS * 1000,
        )
    }

    #[test]
    fn race_first_success_wins() {
        let counter = Arc::new(AtomicUsize::new(0));
        // 阿里快但空答案 → 跳过；腾讯 60ms 成功；Google 400ms 成功（不应等到它）
        let fetcher = fake_fetcher(
            vec![
                ("https://ali.test", 0, Ok(r#"{"Status":0}"#.into())),
                ("https://tencent.test", 60, Ok(r#"{"Status":0,"Answer":[{"type":1,"data":"1.2.3.4"}]}"#.into())),
                ("https://google.test", 400, Ok(r#"{"Status":0,"Answer":[{"type":1,"data":"8.8.8.8"}]}"#.into())),
            ],
            Arc::clone(&counter),
        );
        let resolver = resolver_with(fetcher, Arc::new(ManualClock::new()));
        let start = std::time::Instant::now();
        let ips = resolver.resolve("example.com").expect("race must succeed");
        assert_eq!(
            ips,
            vec!["1.2.3.4".parse::<IpAddr>().unwrap()],
            "首成者（腾讯）胜，不应取到慢端点"
        );
        assert!(
            start.elapsed() < Duration::from_millis(350),
            "不应等待 400ms 的慢端点: {:?}",
            start.elapsed()
        );
        assert_eq!(wait_count(&counter, 3), 3, "三个端点都发起了查询");
        // 结果入缓存
        assert_eq!(resolver.cache_len(), 1);
        assert_eq!(resolver.cached("example.com", 1), Some(ips));
    }

    #[test]
    fn empty_answer_falls_through_to_next_endpoint() {
        let counter = Arc::new(AtomicUsize::new(0));
        let fetcher = fake_fetcher(
            vec![
                ("https://ali.test", 0, Ok(r#"{"Status":3}"#.into())), // NXDOMAIN → 空
                ("https://tencent.test", 0, Ok(r#"{"Answer":[{"type":1,"data":"5.6.7.8"}]}"#.into())),
                ("https://google.test", 0, Err(DohError::Transport("down".into()))),
            ],
            counter,
        );
        let resolver = resolver_with(fetcher, Arc::new(ManualClock::new()));
        let ips = resolver.resolve("example.com").unwrap();
        assert_eq!(ips, vec!["5.6.7.8".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn all_endpoints_fail_returns_error() {
        let counter = Arc::new(AtomicUsize::new(0));
        let fetcher = fake_fetcher(vec![], counter);
        let resolver = resolver_with(fetcher, Arc::new(ManualClock::new()));
        let err = resolver.resolve("example.com").unwrap_err();
        assert!(matches!(err, DohError::Transport(_)), "got: {err:?}");
    }

    #[test]
    fn ttl_cache_expires_at_300s() {
        let counter = Arc::new(AtomicUsize::new(0));
        let fetcher = fake_fetcher(
            vec![(
                "https://ali.test",
                0,
                Ok(r#"{"Answer":[{"type":1,"data":"9.9.9.9"}]}"#.into()),
            )],
            Arc::clone(&counter),
        );
        let clock = Arc::new(ManualClock::new());
        let resolver = resolver_with(fetcher, Arc::clone(&clock) as SharedClock);

        // 第一次：三端点并发竞速（计数器含全部端点调用）
        resolver.resolve("a.test").unwrap();
        assert_eq!(wait_count(&counter, 3), 3);

        // 299s 内：缓存命中（先静置一段确认没有新调用）
        clock.advance_ms(299_000);
        resolver.resolve("a.test").unwrap();
        thread::sleep(Duration::from_millis(50));
        assert_eq!(counter.load(Ordering::SeqCst), 3, "TTL 内必须命中缓存");

        // 300s：过期重查（再次三端点）
        clock.advance_ms(1_000);
        resolver.resolve("a.test").unwrap();
        assert_eq!(wait_count(&counter, 6), 6, "300s 后必须重新查询");

        // clear_cache 立即失效
        resolver.clear_cache();
        assert_eq!(resolver.cache_len(), 0);
        resolver.resolve("a.test").unwrap();
        assert_eq!(wait_count(&counter, 9), 9);
    }

    #[test]
    fn name_normalization_hits_same_cache_entry() {
        let counter = Arc::new(AtomicUsize::new(0));
        let fetcher = fake_fetcher(
            vec![(
                "https://ali.test",
                0,
                Ok(r#"{"Answer":[{"type":1,"data":"9.9.9.9"}]}"#.into()),
            )],
            Arc::clone(&counter),
        );
        let resolver = resolver_with(fetcher, Arc::new(ManualClock::new()));
        resolver.resolve("Example.COM.").unwrap();
        resolver.resolve("example.com").unwrap();
        assert_eq!(resolver.cache_len(), 1, "大小写/尾点归一化同一缓存键");
    }
}
