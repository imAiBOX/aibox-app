//! DoH 解析器：进程内域名解析，绕过污染 DNS，不改系统设置。
//!
//! 设计约束：**网络 IO 与解析分离** —— [`parse_doh_response`] 是纯函数，
//! 单测不联网即可覆盖；[`DohResolver`] 负责并发池/超时/降级。

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

/// DoH 端点基线（并发池顺序 = CN 序优先；GLOBAL 场景 Google 兜底）。
pub const DOH_ENDPOINTS: [&str; 3] = [
    "https://223.5.5.5/dns-query",   // 阿里
    "https://1.12.12.12/dns-query",  // 腾讯
    "https://dns.google/dns-query",  // Google
];

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

/// 进程内 DoH 解析器。 ：端点顺序降级；补并发池 + TTL 300s 缓存。
#[derive(Debug, Clone)]
pub struct DohResolver {
    endpoints: Vec<String>,
    timeout: Duration,
}

impl Default for DohResolver {
    fn default() -> Self {
        Self::new()
    }
}

impl DohResolver {
    pub fn new() -> Self {
        Self {
            endpoints: DOH_ENDPOINTS.iter().map(|s| (*s).to_string()).collect(),
            timeout: Duration::from_secs(5),
        }
    }

    pub fn with_endpoints(endpoints: Vec<String>, timeout: Duration) -> Self {
        Self {
            endpoints,
            timeout,
        }
    }

    /// 解析域名（默认查询 A 记录）。端点顺序尝试，首个拿到非空答案者胜出。
    pub fn resolve(&self, name: &str) -> Result<Vec<IpAddr>, DohError> {
        self.resolve_type(name, 1)
    }

    pub fn resolve_type(&self, name: &str, qtype: u16) -> Result<Vec<IpAddr>, DohError> {
        let mut last_err: Option<DohError> = None;
        for endpoint in &self.endpoints {
            let url = format!("{endpoint}?name={name}&type={qtype}");
            let agent = ureq::AgentBuilder::new()
                .timeout(self.timeout)
                .timeout_connect(self.timeout)
                .build();
            let result = agent
                .get(&url)
                .set("Accept", "application/dns-json")
                .set("User-Agent", "aibox-net/0.0.1")
                .call();
            match result {
                Ok(resp) => match resp.into_string() {
                    Ok(body) => match parse_doh_response(&body) {
                        Ok(ips) if !ips.is_empty() => return Ok(ips),
                        Ok(_) => last_err = Some(DohError::NoAnswer(url.clone())),
                        Err(e) => last_err = Some(e),
                    },
                    Err(e) => last_err = Some(DohError::Transport(e.to_string())),
                },
                Err(ureq::Error::Status(code, _)) => last_err = Some(DohError::Http(code)),
                Err(ureq::Error::Transport(t)) => last_err = Some(DohError::Transport(t.to_string())),
            }
        }
        Err(last_err.unwrap_or_else(|| DohError::NoAnswer("no DoH endpoint configured".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DoH「响应 JSON → IP 列表」纯函数解析（不联网）。
    #[test]
    fn parse_doh_response_returns_ip_list() {
        // 固定的 application/dns-json 响应样本：CNAME + A×2 + AAAA
        let json = r#"{
            "Status": 0,
            "TC": false,
            "RD": true,
            "RA": true,
            "AD": false,
            "CD": false,
            "Question": [{ "name": "github.com.", "type": 1 }],
            "Answer": [
                { "name": "github.com.", "type": 5, "TTL": 59, "data": "github.com." },
                { "name": "github.com.", "type": 1, "TTL": 59, "data": "140.82.113.3" },
                { "name": "github.com.", "type": 1, "TTL": 59, "data": "140.82.114.3" },
                { "name": "github.com.", "type": 28, "TTL": 59, "data": "2606:50c0:8000::153" }
            ]
        }"#;

        let ips = parse_doh_response(json).expect("valid DoH JSON must parse");
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
}
