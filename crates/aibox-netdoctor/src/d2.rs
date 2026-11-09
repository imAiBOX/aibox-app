//! D2 DNS 健康检测：
//! 关键域三路解析比对（系统解析 vs DoH 阿里 vs DoH 腾讯）；
//! 污染特征 = 系统解析成功但与 DoH 不一致**且直连失败**（纯函数判定，可单测）。

use std::net::IpAddr;

use serde_json::json;

use crate::report::{loc, DiagCard, DiagCode, DiagStatus, Fix, FixLevel};
use crate::transport::DoctorDeps;

/// 关键域基线（与矩阵重叠的下载/分发关键域名）。
pub const D2_DOMAINS: [&str; 5] = [
    "github.com",
    "raw.githubusercontent.com",
    "registry.npmjs.org",
    "npmmirror.com",
    "cdn.jsdelivr.net",
];

/// 单域三路解析 + 直连结果。
#[derive(Debug, Clone, PartialEq)]
pub struct DomainDns {
    pub domain: String,
    pub system: Result<Vec<IpAddr>, String>,
    pub doh_ali: Result<Vec<IpAddr>, String>,
    pub doh_tencent: Result<Vec<IpAddr>, String>,
    /// 对该域的 HTTPS 直连探测是否成功（走系统解析结果）。
    pub direct_ok: bool,
    pub direct_error: Option<String>,
}

/// 单域判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DnsVerdict {
    /// 系统与 DoH 一致
    Clean,
    /// 系统解析成功、与 DoH 不一致且直连失败 → 污染特征
    Polluted,
    /// 系统与 DoH 不一致，但直连仍可用（差异未致故障）
    Divergent,
    /// 系统解析失败（R1：进程内 DoH 接管）
    SystemFailed,
    /// 两路 DoH 均不可用，无法比对
    DohUnavailable,
}

impl DnsVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            DnsVerdict::Clean => "clean",
            DnsVerdict::Polluted => "polluted",
            DnsVerdict::Divergent => "divergent",
            DnsVerdict::SystemFailed => "system_failed",
            DnsVerdict::DohUnavailable => "doh_unavailable",
        }
    }

    pub fn status(&self) -> DiagStatus {
        match self {
            DnsVerdict::Clean => DiagStatus::Ok,
            DnsVerdict::Polluted | DnsVerdict::SystemFailed => DiagStatus::Fail,
            DnsVerdict::Divergent | DnsVerdict::DohUnavailable => DiagStatus::Warn,
        }
    }

    pub fn summary(&self) -> crate::report::Localized {
        match self {
            DnsVerdict::Clean => loc(
                "解析正常：系统与 DoH 结果一致",
                "Healthy: system DNS agrees with DoH",
            ),
            DnsVerdict::Polluted => loc(
                "疑似 DNS 污染：系统解析与 DoH 不一致，且直连失败",
                "Likely DNS poisoning: system answer differs from DoH and direct connection fails",
            ),
            DnsVerdict::Divergent => loc(
                "系统解析与 DoH 不一致，但直连仍可用",
                "System DNS differs from DoH, but direct connection still works",
            ),
            DnsVerdict::SystemFailed => loc(
                "系统 DNS 解析失败",
                "System DNS resolution failed",
            ),
            DnsVerdict::DohUnavailable => loc(
                "DoH 解析不可用，无法完成三路比对",
                "DoH unavailable: three-way comparison not possible",
            ),
        }
    }
}

/// 两组 IP 是否一致（**交集非空即一致**：CDN/anycast 多 IP 属正常差异）。
pub fn sets_agree(a: &[IpAddr], b: &[IpAddr]) -> bool {
    a.iter().any(|ip| b.contains(ip))
}

/// 污染判定（纯函数，规则）：
/// 系统解析成功 但 与 DoH（阿里∪腾讯）不一致 且 直连失败 → `Polluted`。
pub fn pollution_verdict(d: &DomainDns) -> DnsVerdict {
    let sys = match &d.system {
        Ok(ips) if !ips.is_empty() => ips,
        _ => return DnsVerdict::SystemFailed,
    };
    let mut doh_all: Vec<IpAddr> = Vec::new();
    if let Ok(ips) = &d.doh_ali {
        doh_all.extend_from_slice(ips);
    }
    if let Ok(ips) = &d.doh_tencent {
        doh_all.extend_from_slice(ips);
    }
    if doh_all.is_empty() {
        return DnsVerdict::DohUnavailable;
    }
    if sets_agree(sys, &doh_all) {
        DnsVerdict::Clean
    } else if d.direct_ok {
        DnsVerdict::Divergent
    } else {
        DnsVerdict::Polluted
    }
}

/// 执行 D2（依赖注入：系统解析 / 两路 DoH / HTTP 直连探测）。
pub fn run(deps: &DoctorDeps) -> DiagCard {
    let mut domains = Vec::with_capacity(D2_DOMAINS.len());
    for domain in D2_DOMAINS {
        let url = format!("https://{domain}/");
        let outcome = deps.http.head(&url, deps.timeout);
        domains.push(DomainDns {
            domain: domain.to_string(),
            system: deps.system_resolve.resolve(domain),
            doh_ali: deps.doh_ali.resolve(domain),
            doh_tencent: deps.doh_tencent.resolve(domain),
            direct_ok: outcome.success,
            direct_error: outcome.error,
        });
    }

    let verdicts: Vec<(&DomainDns, DnsVerdict)> = domains
        .iter()
        .map(|d| (d, pollution_verdict(d)))
        .collect();

    let status = verdicts
        .iter()
        .map(|(_, v)| v.status())
        .reduce(DiagStatus::worse)
        .unwrap_or(DiagStatus::Ok);

    let polluted: Vec<&str> = verdicts
        .iter()
        .filter(|(_, v)| matches!(v, DnsVerdict::Polluted | DnsVerdict::SystemFailed))
        .map(|(d, _)| d.domain.as_str())
        .collect();

    let summary = if polluted.is_empty() {
        match status {
            DiagStatus::Ok => loc(
                "DNS 健康：全部关键域解析一致",
                "DNS healthy: all key domains resolve consistently",
            ),
            _ => loc(
                "DNS 存在差异但暂未致故障（详见明细）",
                "DNS divergence observed, no hard failure yet (see detail)",
            ),
        }
    } else {
        loc(
            format!("{} 个关键域疑似污染/解析失败：{}", polluted.len(), polluted.join(", ")),
            format!(
                "{} key domain(s) likely poisoned or unresolvable: {}",
                polluted.len(),
                polluted.join(", ")
            ),
        )
    };

    let mut fixes = Vec::new();
    if !polluted.is_empty() {
        fixes.push(Fix::new(
            FixLevel::R1,
            "netdoctor.fix.doh接管",
            loc(
                "进程内 DoH 解析替代被污染的系统 DNS（自动，零系统改动）",
                "Switch in-process resolution to DoH, bypassing poisoned system DNS (automatic)",
            ),
            "仅影响 aibox 进程内解析；缓存 TTL 300s；不修改系统 DNS 设置",
        ));
        fixes.push(Fix::new(
            FixLevel::R3,
            "netdoctor.fix.system_dns",
            loc(
                "切换系统公共 DNS 223.5.5.5 / 119.29.29.29（需管理员授权，先备份可还原）",
                "Switch system DNS to 223.5.5.5 / 119.29.29.29 (admin required, snapshot first)",
            ),
                "R3：修改系统网络适配器 DNS 设置，写入 SQLite 修改历史，支持一键还原",
        ));
    }

    DiagCard {
        item: DiagCode::D2,
        status,
        title: DiagCode::D2.title(),
        summary_key: format!("netdoctor.D2.{}", {
            let worst = verdicts
                .iter()
                .map(|(_, v)| *v)
                .reduce(|a, b| {
                    if a.status().rank() >= b.status().rank() {
                        a
                    } else {
                        b
                    }
                })
                .unwrap_or(DnsVerdict::Clean);
            worst.as_str()
        }),
        summary,
        detail: json!({
            "domains": domains.iter().map(|d| {
                let verdict = verdicts
                    .iter()
                    .find(|(x, _)| x.domain == d.domain)
                    .map(|(_, v)| *v)
                    .unwrap_or(DnsVerdict::Clean);
                json!({
                    "domain": d.domain,
                    "system": fmt_ips(&d.system),
                    "doh_ali": fmt_ips(&d.doh_ali),
                    "doh_tencent": fmt_ips(&d.doh_tencent),
                    "direct_ok": d.direct_ok,
                    "direct_error": d.direct_error,
                    "verdict": verdict.as_str(),
                })
            }).collect::<Vec<_>>(),
            "note": "一致 = IP 集合有交集（CDN 多 IP 属正常差异）",
        }),
        timestamp_ms: (deps.now_ms)(),
        fixes,
    }
}

fn fmt_ips(r: &Result<Vec<IpAddr>, String>) -> serde_json::Value {
    match r {
        Ok(ips) => json!(ips.iter().map(|i| i.to_string()).collect::<Vec<_>>()),
        Err(e) => json!({"error": e}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::DiagStatus;
    use crate::transport::{HttpProbe, Resolve};
    use std::net::Ipv4Addr;
    use std::time::Duration;

    fn ip(s: &str) -> IpAddr {
        s.parse::<Ipv4Addr>().unwrap().into()
    }

    fn ok_ips(list: &[&str]) -> Result<Vec<IpAddr>, String> {
        Ok(list.iter().map(|s| ip(s)).collect())
    }

    fn domain(
        system: Result<Vec<IpAddr>, String>,
        ali: Result<Vec<IpAddr>, String>,
        tencent: Result<Vec<IpAddr>, String>,
        direct_ok: bool,
    ) -> DomainDns {
        DomainDns {
            domain: "github.com".into(),
            system,
            doh_ali: ali,
            doh_tencent: tencent,
            direct_ok,
            direct_error: if direct_ok { None } else { Some("connect failed".into()) },
        }
    }

    #[test]
    fn pollution_requires_mismatch_and_direct_failure() {
        let sys = ok_ips(&["20.205.243.166"]);
        let doh = ok_ips(&["140.82.113.3"]);
        // 系统与 DoH 不一致 + 直连失败 → 污染
        assert_eq!(
            pollution_verdict(&domain(sys.clone(), doh.clone(), doh.clone(), false)),
            DnsVerdict::Polluted
        );
        // 同样不一致但直连可用 → divergent（不误判污染）
        assert_eq!(
            pollution_verdict(&domain(sys.clone(), doh.clone(), doh.clone(), true)),
            DnsVerdict::Divergent
        );
        // 一致 → clean（无论直连）
        assert_eq!(
            pollution_verdict(&domain(sys.clone(), sys.clone(), doh.clone(), false)),
            DnsVerdict::Clean
        );
    }

    #[test]
    fn pollution_edge_cases() {
        // 系统解析失败 → system_failed
        assert_eq!(
            pollution_verdict(&domain(
                Err("no such host".into()),
                ok_ips(&["1.2.3.4"]),
                ok_ips(&["1.2.3.5"]),
                false
            )),
            DnsVerdict::SystemFailed
        );
        // 系统返回空列表也算失败
        assert_eq!(
            pollution_verdict(&domain(Ok(vec![]), ok_ips(&["1.2.3.4"]), ok_ips(&["1.2.3.5"]), false)),
            DnsVerdict::SystemFailed
        );
        // 两路 DoH 都不可用 → doh_unavailable
        assert_eq!(
            pollution_verdict(&domain(ok_ips(&["1.2.3.4"]), Err("timeout".into()), Err("timeout".into()), false)),
            DnsVerdict::DohUnavailable
        );
        // 系统命中阿里但不命中腾讯（union 交集）→ clean
        assert_eq!(
            pollution_verdict(&domain(
                ok_ips(&["9.9.9.9"]),
                ok_ips(&["9.9.9.9"]),
                ok_ips(&["8.8.8.8"]),
                false
            )),
            DnsVerdict::Clean
        );
        // 两路 DoH 都成功但都为空 → unavailable
        assert_eq!(
            pollution_verdict(&domain(ok_ips(&["1.2.3.4"]), Ok(vec![]), Ok(vec![]), false)),
            DnsVerdict::DohUnavailable
        );
    }

    #[test]
    fn sets_agree_by_intersection() {
        assert!(sets_agree(&[ip("1.1.1.1"), ip("2.2.2.2")], &[ip("2.2.2.2")]));
        assert!(!sets_agree(&[ip("1.1.1.1")], &[ip("2.2.2.2")]));
        assert!(!sets_agree(&[], &[ip("1.1.1.1")]));
    }

    struct FakeResolve(Vec<IpAddr>, bool);
    impl Resolve for FakeResolve {
        fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
            if self.1 {
                Ok(self.0.clone())
            } else {
                Err("mock resolve failure".into())
            }
        }
    }

    struct AlwaysFailProbe;
    impl HttpProbe for AlwaysFailProbe {
        fn head(&self, _url: &str, _t: Duration) -> aibox_net::ProbeOutcome {
            aibox_net::ProbeOutcome {
                success: false,
                ttfb_ms: 1,
                status: None,
                error: Some("mock direct failure".into()),
            }
        }
    }

    #[test]
    fn d2_run_flags_polluted_domains() {
        // 系统解析全部返回同一个假 IP；DoH 返回真实 IP；直连全部失败 → 全部污染
        let deps = DoctorDeps {
            system_resolve: Box::new(FakeResolve(vec![ip("6.6.6.6")], true)),
            doh_ali: Box::new(FakeResolve(vec![ip("20.205.243.166")], true)),
            doh_tencent: Box::new(FakeResolve(vec![ip("140.82.113.3")], true)),
            http: Box::new(AlwaysFailProbe),
            ..DoctorDeps::default()
        };
        let card = run(&deps);
        assert_eq!(card.item, DiagCode::D2);
        assert_eq!(card.status, DiagStatus::Fail);
        assert_eq!(card.detail["domains"].as_array().unwrap().len(), 5);
        assert_eq!(card.detail["domains"][0]["verdict"], "polluted");
        assert_eq!(card.detail["domains"][0]["system"][0], "6.6.6.6");
        // 修复分级：R1 自动 DoH 接管 + R3 需授权改系统 DNS
        let levels: Vec<&str> = card.fixes.iter().map(|f| f.level.mode()).collect();
        assert_eq!(levels, ["auto", "authorize"]);
    }

    struct AlwaysOkProbe;
    impl HttpProbe for AlwaysOkProbe {
        fn head(&self, _url: &str, _t: Duration) -> aibox_net::ProbeOutcome {
            aibox_net::ProbeOutcome {
                success: true,
                ttfb_ms: 5,
                status: Some(200),
                error: None,
            }
        }
    }

    #[test]
    fn d2_run_clean_when_consistent() {
        let deps = DoctorDeps {
            system_resolve: Box::new(FakeResolve(vec![ip("20.205.243.166")], true)),
            doh_ali: Box::new(FakeResolve(vec![ip("20.205.243.166")], true)),
            doh_tencent: Box::new(FakeResolve(vec![ip("140.82.113.3")], true)),
            http: Box::new(AlwaysOkProbe),
            ..DoctorDeps::default()
        };
        let card = run(&deps);
        assert_eq!(card.status, DiagStatus::Ok);
        assert_eq!(card.summary_key, "netdoctor.D2.clean");
        assert!(card.fixes.is_empty());
    }
}
