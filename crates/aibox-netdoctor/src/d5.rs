//! D5 阻断层定位：
//! 对被 D2 标记污染（或调用方指定）的域，取 DoH 解析结果 IP 做**固定 IP 直连**
//! （SNI/Host 仍为原域名，经注入的 `TlsProbe`），区分阻断发生在哪一层：
//! - `dns_pollution`：DoH IP 直连成功 → IP 层没被断，是系统 DNS 被污染（R1 DoH 接管即愈）；
//! - `ip_blocking`：DoH IP 直连也失败 → IP/传输层被阻断（需走代理/通道降级）；
//! - `ok`：系统解析直连即成功（无阻断）；
//! - `unknown`：DoH 无结果且系统直连失败，无法判定。

use std::net::IpAddr;
use std::thread;

use serde_json::json;

use crate::report::{loc, DiagCard, DiagCode, DiagStatus, Fix, FixLevel};
use crate::transport::DoctorDeps;

/// 阻断层定位结论。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockageLayer {
    /// 系统 DNS 被污染（DoH IP 直连可用）
    DnsPollution,
    /// IP/传输层被阻断（DoH IP 直连也失败）
    IpBlocking,
    /// 无阻断（系统直连成功）
    Ok,
    /// 无法判定（DoH 无结果且系统直连失败）
    Unknown,
}

impl BlockageLayer {
    pub fn as_str(&self) -> &'static str {
        match self {
            BlockageLayer::DnsPollution => "dns_pollution",
            BlockageLayer::IpBlocking => "ip_blocking",
            BlockageLayer::Ok => "ok",
            BlockageLayer::Unknown => "unknown",
        }
    }

    pub fn status(&self) -> DiagStatus {
        match self {
            BlockageLayer::IpBlocking => DiagStatus::Fail,
            BlockageLayer::DnsPollution | BlockageLayer::Unknown => DiagStatus::Warn,
            BlockageLayer::Ok => DiagStatus::Ok,
        }
    }

    pub fn summary(&self) -> crate::report::Localized {
        match self {
            BlockageLayer::DnsPollution => loc(
                "DNS 层污染：DoH IP 直连可用，进程内 DoH 接管即可恢复",
                "DNS-layer poisoning: direct connection to DoH IPs works; in-process DoH takeover recovers",
            ),
            BlockageLayer::IpBlocking => loc(
                "IP/传输层被阻断：DoH IP 直连也失败（需代理通道/通道降级）",
                "IP/transport blocked: even direct connections to DoH IPs fail (needs proxy/fallback channel)",
            ),
            BlockageLayer::Ok => loc(
                "无阻断：直连正常",
                "No blocking: direct connection works",
            ),
            BlockageLayer::Unknown => loc(
                "无法判定：DoH 无结果且系统直连失败",
                "Undetermined: no DoH answers and system direct connection fails",
            ),
        }
    }
}

/// 单域直连探测结果（D5 分类输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirectResults {
    /// 系统解析直连（`https://<domain>/`）是否成功。
    pub system_ok: bool,
    /// DoH 结果 IP 固定直连是否成功（任一 IP 成功即为 true）。
    pub doh_ip_ok: bool,
}

/// 阻断层分类（纯函数，可单测）。判定矩阵：
///
/// | 系统直连 | DoH IP | doh_ips | 结论 |
/// |---|---|---|---|
/// | 成功 | （无需测） | 任意 | `ok`（无阻断） |
/// | 失败 | 成功 | 非空 | `dns_pollution`（IP 层没断，系统 DNS 被污染） |
/// | 失败 | 失败 | 非空 | `ip_blocking`（IP/传输层被阻断） |
/// | 失败 | — | 空 | `unknown`（DoH 无结果，无法判定） |
pub fn classify_blockage(doh_ips: &[IpAddr], direct: &DirectResults) -> BlockageLayer {
    if direct.system_ok {
        return BlockageLayer::Ok;
    }
    if doh_ips.is_empty() {
        return BlockageLayer::Unknown;
    }
    if direct.doh_ip_ok {
        BlockageLayer::DnsPollution
    } else {
        BlockageLayer::IpBlocking
    }
}

/// 单域 D5 结果。
#[derive(Debug, Clone)]
pub struct DomainBlockage {
    pub domain: String,
    pub system_ok: bool,
    pub system_error: Option<String>,
    pub doh_ips: Vec<IpAddr>,
    pub doh_ip_ok: bool,
    pub pinned_errors: Vec<String>,
    pub layer: BlockageLayer,
}

/// 对单域执行 D5 定位（探测原语全部经 `deps` 注入）。
pub fn probe_domain(domain: &str, deps: &DoctorDeps) -> DomainBlockage {
    let url = format!("https://{domain}/");
    let direct = deps.http.head(&url, deps.timeout);
    let system_ok = direct.success;

    if system_ok {
        // 系统路径直连即成功 → 无阻断，无需 DoH/IP 探测
        return DomainBlockage {
            domain: domain.to_string(),
            system_ok: true,
            system_error: None,
            doh_ips: Vec::new(),
            doh_ip_ok: false,
            pinned_errors: Vec::new(),
            layer: classify_blockage(&[], &DirectResults { system_ok: true, doh_ip_ok: false }),
        };
    }

    // DoH 双路并集（去重；与 D2 同源：阿里 + 腾讯，基线）
    let mut doh_ips: Vec<IpAddr> = Vec::new();
    for result in [
        deps.doh_ali.resolve(domain),
        deps.doh_tencent.resolve(domain),
    ] {
        if let Ok(ips) = result {
            for ip in ips {
                if !doh_ips.contains(&ip) {
                    doh_ips.push(ip);
                }
            }
        }
    }

    if doh_ips.is_empty() {
        return DomainBlockage {
            domain: domain.to_string(),
            system_ok: false,
            system_error: direct.error,
            doh_ips,
            doh_ip_ok: false,
            pinned_errors: Vec::new(),
            layer: classify_blockage(&[], &DirectResults::default()),
        };
    }

    // 对每个 DoH IP 固定直连（SNI/Host = 原域名）；任一成功即 IP 层未被断
    let mut doh_ip_ok = false;
    let mut pinned_errors = Vec::new();
    for ip in &doh_ips {
        let attempt = deps.tls.head_tls(domain, Some(*ip), deps.timeout);
        if attempt.success {
            doh_ip_ok = true;
            break;
        }
        if let Some(err) = attempt.error {
            pinned_errors.push(err);
        }
    }

    let layer = classify_blockage(&doh_ips, &DirectResults { system_ok, doh_ip_ok });
    DomainBlockage {
        domain: domain.to_string(),
        system_ok,
        system_error: direct.error,
        doh_ips,
        doh_ip_ok,
        pinned_errors,
        layer,
    }
}

fn empty_blockage(domain: &str, error: &str) -> DomainBlockage {
    DomainBlockage {
        domain: domain.to_string(),
        system_ok: false,
        system_error: Some(error.to_string()),
        doh_ips: Vec::new(),
        doh_ip_ok: false,
        pinned_errors: Vec::new(),
        layer: BlockageLayer::Unknown,
    }
}

/// 执行 D5（对给定域列表；并发逐域探测）。`domains` 为空 → `no_domains` warn 卡。
pub fn run_with_domains<S: AsRef<str>>(domains: &[S], deps: &DoctorDeps) -> DiagCard {
    if domains.is_empty() {
        return DiagCard {
            item: DiagCode::D5,
            status: DiagStatus::Warn,
            title: DiagCode::D5.title(),
            summary_key: "netdoctor.D5.no_domains".to_string(),
            summary: loc(
                "未提供待定位域名（由调用方传入，或走默认关键域）",
                "No domains supplied for blockage analysis (caller-provided or default key domains)",
            ),
            detail: json!({
                "domains": [],
                "counts": {"dns_pollution": 0, "ip_blocking": 0, "ok": 0, "unknown": 0},
            }),
            timestamp_ms: (deps.now_ms)(),
            fixes: Vec::new(),
        };
    }

    let results: Vec<DomainBlockage> = thread::scope(|s| {
        let handles: Vec<_> = domains
            .iter()
            .map(|d| {
                let domain = d.as_ref().to_string();
                let handle = {
                    let domain = domain.clone();
                    s.spawn(move || probe_domain(&domain, deps))
                };
                (domain, handle)
            })
            .collect();
        handles
            .into_iter()
            .map(|(domain, handle)| {
                handle
                    .join()
                    .unwrap_or_else(|_| empty_blockage(&domain, "probe thread panicked"))
            })
            .collect()
    });

    aggregate(results, deps)
}

/// 执行 D5（默认关键域 = D2 域列表；生产中调用方可把 D2 判污染的域传入
/// [`run_with_domains`]）。
pub fn run(deps: &DoctorDeps) -> DiagCard {
    run_with_domains(&crate::d2::D2_DOMAINS, deps)
}

fn aggregate(results: Vec<DomainBlockage>, deps: &DoctorDeps) -> DiagCard {
    let mut counts = [0usize; 4]; // dns_pollution, ip_blocking, ok, unknown
    for r in &results {
        match r.layer {
            BlockageLayer::DnsPollution => counts[0] += 1,
            BlockageLayer::IpBlocking => counts[1] += 1,
            BlockageLayer::Ok => counts[2] += 1,
            BlockageLayer::Unknown => counts[3] += 1,
        }
    }
    let worst = if counts[1] > 0 {
        BlockageLayer::IpBlocking
    } else if counts[0] > 0 {
        BlockageLayer::DnsPollution
    } else if counts[3] > 0 {
        BlockageLayer::Unknown
    } else {
        BlockageLayer::Ok
    };
    let status = results
        .iter()
        .map(|r| r.layer.status())
        .reduce(DiagStatus::worse)
        .unwrap_or(DiagStatus::Ok);

    let list = |layer: BlockageLayer| -> String {
        results
            .iter()
            .filter(|r| r.layer == layer)
            .map(|r| r.domain.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let summary = match worst {
        BlockageLayer::IpBlocking => loc(
            format!(
                "{} 个域在 IP/传输层被阻断（换代理/通道可绕过）：{}",
                counts[1],
                list(BlockageLayer::IpBlocking)
            ),
            format!(
                "{} domain(s) blocked at IP/transport layer (switch proxy/channel to bypass): {}",
                counts[1],
                list(BlockageLayer::IpBlocking)
            ),
        ),
        BlockageLayer::DnsPollution => loc(
            format!(
                "{} 个域为 DNS 层污染（DoH 直连可用，R1 进程内 DoH 接管即恢复）：{}",
                counts[0],
                list(BlockageLayer::DnsPollution)
            ),
            format!(
                "{} domain(s) poisoned at DNS layer (DoH IPs reachable; in-process DoH takeover recovers): {}",
                counts[0],
                list(BlockageLayer::DnsPollution)
            ),
        ),
        BlockageLayer::Unknown => loc(
            format!("{} 个域因 DoH 无结果无法判定：{}", counts[3], list(BlockageLayer::Unknown)),
            format!(
                "{} domain(s) undetermined (no DoH answers): {}",
                counts[3],
                list(BlockageLayer::Unknown)
            ),
        ),
        BlockageLayer::Ok => loc(
            format!("阻断层定位：全部 {} 个域直连正常", results.len()),
            format!(
                "Blockage analysis: all {} domain(s) reachable directly",
                results.len()
            ),
        ),
    };

    let mut fixes = Vec::new();
    if counts[1] > 0 {
        fixes.push(Fix::new(
            FixLevel::R1,
            "netdoctor.fix.proxy_fallback",
            loc(
                "启用代理通道或按 D4 测速自动降级到可用通道（进程内，自动）",
                "Use the proxy channel or auto-fallback to a working channel per D4 (in-process, automatic)",
            ),
            "IP/传输层被断时 DoH 也救不了；仅影响 aibox 进程内下载选路，不改系统设置",
        ));
    }
    if counts[0] > 0 {
        fixes.push(Fix::new(
            FixLevel::R1,
            "netdoctor.fix.doh接管",
            loc(
                "进程内 DoH 解析替代被污染的系统 DNS（自动，零系统改动）",
                "Switch in-process resolution to DoH, bypassing poisoned system DNS (automatic)",
            ),
            "仅影响 aibox 进程内解析；缓存 TTL 300s；不修改系统 DNS 设置",
        ));
    }

    DiagCard {
        item: DiagCode::D5,
        status,
        title: DiagCode::D5.title(),
        summary_key: format!("netdoctor.D5.{}", worst.as_str()),
        summary,
        detail: json!({
            "domains": results.iter().map(|r| json!({
                "domain": r.domain,
                "system_ok": r.system_ok,
                "system_error": r.system_error,
                "doh_ips": r.doh_ips.iter().map(|i| i.to_string()).collect::<Vec<_>>(),
                "doh_ip_ok": r.doh_ip_ok,
                "pinned_errors": r.pinned_errors,
                "blockage": r.layer.as_str(),
            })).collect::<Vec<_>>(),
            "counts": {
                "dns_pollution": counts[0],
                "ip_blocking": counts[1],
                "ok": counts[2],
                "unknown": counts[3],
            },
            "note": "定位方法：对 DoH 结果 IP 固定直连（SNI/Host=原域名）；成功=IP 层未被断",
        }),
        timestamp_ms: (deps.now_ms)(),
        fixes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::DiagStatus;
    use crate::transport::{DoctorDeps, HttpProbe, Resolve, TlsProbe};
    use std::net::Ipv4Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    fn ip(s: &str) -> IpAddr {
        s.parse::<Ipv4Addr>().unwrap().into()
    }

    fn ips(list: &[&str]) -> Vec<IpAddr> {
        list.iter().map(|s| ip(s)).collect()
    }

    // ── classify_blockage 矩阵 ────────────────────────────────────────────

    #[test]
    fn classify_matrix_full() {
        let doh = ips(&["1.1.1.1"]);
        // 系统直连成功 → ok（无需 DoH）
        assert_eq!(
            classify_blockage(&[], &DirectResults { system_ok: true, doh_ip_ok: false }),
            BlockageLayer::Ok
        );
        assert_eq!(
            classify_blockage(&doh, &DirectResults { system_ok: true, doh_ip_ok: false }),
            BlockageLayer::Ok
        );
        // 系统失败 + DoH 空 → unknown
        assert_eq!(
            classify_blockage(&[], &DirectResults::default()),
            BlockageLayer::Unknown
        );
        // 系统失败 + DoH IP 直连成功 → dns_pollution
        assert_eq!(
            classify_blockage(&doh, &DirectResults { system_ok: false, doh_ip_ok: true }),
            BlockageLayer::DnsPollution
        );
        // 系统失败 + DoH IP 直连也失败 → ip_blocking
        assert_eq!(
            classify_blockage(&doh, &DirectResults { system_ok: false, doh_ip_ok: false }),
            BlockageLayer::IpBlocking
        );
        // 空 IP 列表 + 系统失败 + doh_ip_ok=true 的矛盾输入 → 仍按「无 DoH 结果」判 unknown
        assert_eq!(
            classify_blockage(&[], &DirectResults { system_ok: false, doh_ip_ok: true }),
            BlockageLayer::Unknown
        );
    }

    #[test]
    fn layer_status_and_str() {
        assert_eq!(BlockageLayer::IpBlocking.status(), DiagStatus::Fail);
        assert_eq!(BlockageLayer::DnsPollution.status(), DiagStatus::Warn);
        assert_eq!(BlockageLayer::Unknown.status(), DiagStatus::Warn);
        assert_eq!(BlockageLayer::Ok.status(), DiagStatus::Ok);
        assert_eq!(BlockageLayer::DnsPollution.as_str(), "dns_pollution");
        assert_eq!(BlockageLayer::IpBlocking.as_str(), "ip_blocking");
        assert_eq!(BlockageLayer::Ok.as_str(), "ok");
        assert_eq!(BlockageLayer::Unknown.as_str(), "unknown");
        // 中英双语 summary 非空
        for l in [
            BlockageLayer::DnsPollution,
            BlockageLayer::IpBlocking,
            BlockageLayer::Ok,
            BlockageLayer::Unknown,
        ] {
            let s = l.summary();
            assert!(!s.zh.is_empty() && !s.en.is_empty());
        }
    }

    // ── 注入 transport 的完整 run 流程 ────────────────────────────────────

    struct FixedHttp(bool, Arc<AtomicUsize>);
    impl HttpProbe for FixedHttp {
        fn head(&self, _url: &str, _t: Duration) -> aibox_net::ProbeOutcome {
            self.1.fetch_add(1, Ordering::SeqCst);
            aibox_net::ProbeOutcome {
                success: self.0,
                ttfb_ms: 5,
                status: if self.0 { Some(200) } else { None },
                error: if self.0 { None } else { Some("mock system direct failure".into()) },
            }
        }
    }

    struct FixedDoh(Vec<IpAddr>, bool);
    impl Resolve for FixedDoh {
        fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
            if self.1 {
                Ok(self.0.clone())
            } else {
                Err("mock doh down".into())
            }
        }
    }

    struct FixedTls(bool, Arc<AtomicUsize>);
    impl TlsProbe for FixedTls {
        fn head_tls(&self, _host: &str, _ip: Option<IpAddr>, _t: Duration) -> crate::transport::TlsAttempt {
            self.1.fetch_add(1, Ordering::SeqCst);
            if self.0 {
                crate::transport::TlsAttempt::ok(200, None)
            } else {
                crate::transport::TlsAttempt::fail("mock pinned-ip failure")
            }
        }
    }

    fn deps_with(
        system_ok: bool,
        doh_ok: bool,
        pinned_ok: bool,
    ) -> (DoctorDeps, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let http_calls = Arc::new(AtomicUsize::new(0));
        let tls_calls = Arc::new(AtomicUsize::new(0));
        let deps = DoctorDeps {
            http: Box::new(FixedHttp(system_ok, http_calls.clone())),
            tls: Box::new(FixedTls(pinned_ok, tls_calls.clone())),
            system_resolve: Box::new(FixedDoh(vec![ip("9.9.9.9")], true)),
            doh_ali: Box::new(FixedDoh(vec![ip("20.205.243.166")], doh_ok)),
            doh_tencent: Box::new(FixedDoh(vec![ip("140.82.113.3")], doh_ok)),
            ..DoctorDeps::default()
        };
        (deps, http_calls, tls_calls)
    }

    #[test]
    fn d5_run_dns_pollution_when_pinned_ok() {
        // 系统直连失败 + DoH 有结果 + DoH IP 直连成功 → dns_pollution / warn
        let (deps, _, tls_calls) = deps_with(false, true, true);
        let card = run(&deps);
        assert_eq!(card.item, DiagCode::D5);
        assert_eq!(card.status, DiagStatus::Warn);
        assert_eq!(card.summary_key, "netdoctor.D5.dns_pollution");
        assert_eq!(card.detail["counts"]["dns_pollution"], 5);
        assert_eq!(card.detail["counts"]["ip_blocking"], 0);
        let domains = card.detail["domains"].as_array().unwrap();
        assert_eq!(domains.len(), 5);
        assert_eq!(domains[0]["blockage"], "dns_pollution");
        assert_eq!(domains[0]["doh_ips"][0], "20.205.243.166");
        assert!(domains[0]["doh_ip_ok"].as_bool().unwrap());
        // R1 DoH 接管建议
        assert_eq!(card.fixes.len(), 1);
        assert!(card.fixes[0].level.is_auto());
        // 每域 1 次 pinned 探测（成功即 break）
        assert_eq!(tls_calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn d5_run_ip_blocking_when_pinned_fails() {
        // 系统直连失败 + DoH 有结果 + DoH IP 直连也失败 → ip_blocking / fail
        let (deps, _, _) = deps_with(false, true, false);
        let card = run_with_domains(&["github.com"], &deps);
        assert_eq!(card.status, DiagStatus::Fail);
        assert_eq!(card.summary_key, "netdoctor.D5.ip_blocking");
        assert_eq!(card.detail["counts"]["ip_blocking"], 1);
        let domains = card.detail["domains"].as_array().unwrap();
        assert_eq!(domains.len(), 1);
        assert_eq!(domains[0]["blockage"], "ip_blocking");
        assert!(domains[0]["system_error"].is_string());
        // 修复：R1 代理/通道降级（auto）
        assert_eq!(card.fixes.len(), 1);
        assert_eq!(card.fixes[0].level, FixLevel::R1);
        assert!(card.fixes[0].level.is_auto());
    }

    #[test]
    fn d5_run_ok_skips_doh_and_pinned() {
        // 系统直连成功 → ok，且不触发 DoH/pinned 探测
        let (deps, http_calls, tls_calls) = deps_with(true, false, false);
        let card = run(&deps);
        assert_eq!(card.status, DiagStatus::Ok);
        assert_eq!(card.summary_key, "netdoctor.D5.ok");
        assert_eq!(card.detail["counts"]["ok"], 5);
        assert!(card.fixes.is_empty());
        assert_eq!(http_calls.load(Ordering::SeqCst), 5);
        assert_eq!(tls_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn d5_run_unknown_when_doh_down() {
        // 系统直连失败 + DoH 双路失败 → unknown / warn
        let (deps, _, tls_calls) = deps_with(false, false, false);
        let card = run_with_domains(&["github.com", "npmmirror.com"], &deps);
        assert_eq!(card.status, DiagStatus::Warn);
        assert_eq!(card.summary_key, "netdoctor.D5.unknown");
        assert_eq!(card.detail["counts"]["unknown"], 2);
        assert_eq!(tls_calls.load(Ordering::SeqCst), 0);
        assert!(card.fixes.is_empty());
    }

    #[test]
    fn d5_empty_domains_is_warn() {
        let deps = DoctorDeps::default();
        let card = run_with_domains(&[] as &[&str], &deps);
        assert_eq!(card.status, DiagStatus::Warn);
        assert_eq!(card.summary_key, "netdoctor.D5.no_domains");
        assert!(card.fixes.is_empty());
    }

    /// 卡片序列化到 TS 边界（形状与 D0-D4 一致）。
    #[test]
    fn d5_card_serializes() {
        let (deps, _, _) = deps_with(false, true, false);
        let card = run_with_domains(&["github.com"], &deps);
        let v = serde_json::to_value(&card).unwrap();
        assert_eq!(v["item"], "D5");
        assert_eq!(v["status"], "fail");
        assert!(v["summary"]["zh"].is_string());
        assert!(v["summary"]["en"].is_string());
        assert!(v["detail"]["domains"].is_array());
        assert!(v["fixes"][0]["level"] == "R1");
    }
}
