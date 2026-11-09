//! aibox-netdoctor — Network Doctor 网络急救箱。
//!
//! 本 crate **必须可脱离 Tauri 独立编译与单测**；全部网络探测经
//! [`DoctorDeps`] 注入，单测离线 mock（本地 TcpListener）。
//!
//! 诊断项：
//! - [`d0`] D0 环境锚测（baidu/github 204 → 纯国内/国际/疑似代理中转）
//! - [`d1`] D1 系统代理审计（env / WinINET / WinHTTP；死代理判定；只读）
//! - [`d2`] D2 DNS 健康检测（系统 vs DoH 阿里 vs DoH 腾讯 三路比对 + 污染特征）
//! - [`d3`] D3 hosts 审计（关键域条目分类；**不自动修改**）
//! - [`d4`] D4 通道实测（矩阵并发测速表；矩阵由调用方传入）
//!
//! 修复分级：报告只产出 R1（自动）/ R2（建议）/ R3（需授权）建议引用，
//! **本 crate 不执行任何系统级修改**；主流程只依赖 R1。
//!
//! ```no_run
//! let report = aibox_netdoctor::run(None, &aibox_netdoctor::DoctorDeps::default());
//! println!("{}", serde_json::to_string_pretty(&report).unwrap());
//! ```

pub mod d0;
pub mod d1;
pub mod d2;
pub mod d3;
pub mod d4;
pub mod report;
pub mod transport;

pub use d0::{classify_env, D0Config, D0Thresholds, EnvClass};
pub use d1::{classify_proxy_health, parse_proxy_endpoint, D1Config, ProxyHealth, ProxyKind, ProxySetting};
pub use d2::{pollution_verdict, sets_agree, DnsVerdict, DomainDns};
pub use d3::{classify_entry, parse_hosts, ClassifyRules, CriticalClass, EntryVerdict, HostsEntry};
pub use d4::{probe_matrix, sort_rows, ChannelRow, ChannelTarget};
pub use report::{
    loc, DiagCard, DiagCode, DiagReport, DiagStatus, Fix, FixLevel, Localized,
};
pub use transport::{
    default_hosts_path, DefaultProxySource, DoctorDeps, DohResolve, FileHosts, HeadProbe,
    HostsSource, HttpProbe, ProxySource, Resolve, StdTcpProbe, SystemResolve, TcpProbe,
};

/// 运行体检（D0-D4）。
///
/// - `items`：`None` = 全量 D0-D4；否则按给定子集执行（输出仍按 D0→D4 规范排序）。
/// - `deps`：可注入依赖（单测传 mock；生产用 [`DoctorDeps::default`]）。
///
/// D4 矩阵为空表（`no_targets`）；需要 D4 测速表时用 [`run_with_matrix`] 传入矩阵。
pub fn run(items: Option<&[DiagCode]>, deps: &DoctorDeps) -> DiagReport {
    run_with_matrix(items, deps, &[])
}

/// 同 [`run`]，但 D4 使用调用方传入的通道矩阵（矩阵由 NetLayer/registry 展开，
/// 本 crate 不硬编码）。
pub fn run_with_matrix(
    items: Option<&[DiagCode]>,
    deps: &DoctorDeps,
    d4_targets: &[ChannelTarget],
) -> DiagReport {
    let codes: Vec<DiagCode> = match items {
        Some(list) => {
            let mut picked: Vec<DiagCode> = list.to_vec();
            picked.dedup();
            picked.sort_by_key(|c| c.order());
            picked
        }
        None => DiagCode::all().to_vec(),
    };

    let cards: Vec<DiagCard> = codes
        .iter()
        .map(|code| match code {
            DiagCode::D0 => d0::run(deps),
            DiagCode::D1 => d1::run(deps),
            DiagCode::D2 => d2::run(deps),
            DiagCode::D3 => d3::run(deps),
            DiagCode::D4 => d4::run(d4_targets, deps),
        })
        .collect();

    DiagReport::new(cards, (deps.now_ms)())
}

/// 便捷入口：按字符串子集执行（TS 边界 `netdoctor_run(items?: ('D0'|... )[])`）。
pub fn run_names(items: Option<&[String]>, deps: &DoctorDeps) -> Result<DiagReport, String> {
    run_names_with_matrix(items, deps, &[])
}

/// [`run_names`] + D4 矩阵。
pub fn run_names_with_matrix(
    items: Option<&[String]>,
    deps: &DoctorDeps,
    d4_targets: &[ChannelTarget],
) -> Result<DiagReport, String> {
    match items {
        None => Ok(run_with_matrix(None, deps, d4_targets)),
        Some(list) => {
            let mut codes = Vec::with_capacity(list.len());
            for name in list {
                let code = DiagCode::parse(name)
                    .ok_or_else(|| format!("未知诊断项: {name}（合法值 D0-D4）"))?;
                if !codes.contains(&code) {
                    codes.push(code);
                }
            }
            Ok(run_with_matrix(Some(&codes), deps, d4_targets))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use std::time::Duration;

    use aibox_net::ProbeOutcome;

    use crate::d1::ProxySetting;
    use crate::transport::{HttpProbe, ProxySource, Resolve, TcpProbe};

    struct OkProbe;
    impl HttpProbe for OkProbe {
        fn head(&self, _url: &str, _t: Duration) -> ProbeOutcome {
            ProbeOutcome {
                success: true,
                ttfb_ms: 7,
                status: Some(200),
                error: None,
            }
        }
    }

    struct NoTcp;
    impl TcpProbe for NoTcp {
        fn port_open(&self, _h: &str, _p: u16, _t: Duration) -> bool {
            false
        }
        fn http_connect(&self, _h: &str, _p: u16, _th: &str, _tp: u16, _t: Duration) -> Result<(), String> {
            Err("no".into())
        }
    }

    struct IpResolve(IpAddr);
    impl Resolve for IpResolve {
        fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>, String> {
            Ok(vec![self.0])
        }
    }

    struct EmptyHosts;
    impl crate::transport::HostsSource for EmptyHosts {
        fn read(&self) -> Result<String, String> {
            Ok("127.0.0.1 localhost\n".into())
        }
        fn location(&self) -> String {
            "memory".into()
        }
    }

    struct NoProxy;
    impl ProxySource for NoProxy {
        fn settings(&self) -> Vec<ProxySetting> {
            vec![]
        }
        fn no_proxy(&self) -> Option<String> {
            None
        }
        fn notes(&self) -> Vec<String> {
            vec!["mock".into()]
        }
    }

    fn mock_deps() -> DoctorDeps {
        let ip: IpAddr = "1.2.3.4".parse().unwrap();
        DoctorDeps {
            http: Box::new(OkProbe),
            tcp: Box::new(NoTcp),
            system_resolve: Box::new(IpResolve(ip)),
            doh_ali: Box::new(IpResolve(ip)),
            doh_tencent: Box::new(IpResolve(ip)),
            hosts: Box::new(EmptyHosts),
            proxy_source: Box::new(NoProxy),
            timeout: Duration::from_millis(50),
            now_ms: Box::new(|| 1_700_000_000_000),
        }
    }

    #[test]
    fn full_run_produces_all_five_cards_in_order() {
        let report = run(None, &mock_deps());
        assert_eq!(report.schema, "aibox-netdoctor/v1");
        assert_eq!(report.generated_at_ms, 1_700_000_000_000);
        let codes: Vec<&str> = report.items.iter().map(|c| c.item.as_str()).collect();
        assert_eq!(codes, ["D0", "D1", "D2", "D3", "D4"]);
        // 全 mock 环境下：D0 双通且快 → warn(suspicious_relay)；D1 无代理 → ok；
        // D2 系统=DoH 一致 → ok；D3 无关键域 → ok；D4 无矩阵 → warn。
        assert_eq!(report.get(DiagCode::D1).unwrap().status, DiagStatus::Ok);
        assert_eq!(report.get(DiagCode::D2).unwrap().status, DiagStatus::Ok);
        assert_eq!(report.get(DiagCode::D3).unwrap().status, DiagStatus::Ok);
        assert_eq!(report.get(DiagCode::D4).unwrap().status, DiagStatus::Warn);
        // 报告整体可序列化到 TS
        let v = serde_json::to_value(&report).expect("serialize report");
        assert_eq!(v["items"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn run_subset_and_names() {
        let deps = mock_deps();
        let report = run(Some(&[DiagCode::D3, DiagCode::D0]), &deps);
        let codes: Vec<&str> = report.items.iter().map(|c| c.item.as_str()).collect();
        assert_eq!(codes, ["D0", "D3"]); // 规范排序

        let report2 = run_names(
            Some(&["D2".to_string(), "d2".to_string()]),
            &deps,
        );
        // 大小写不敏感 + 去重；D9 报错
        assert!(run_names(Some(&["D9".into()]), &deps).is_err());
        let r2 = report2.unwrap();
        assert_eq!(r2.items.len(), 1);
        assert_eq!(r2.items[0].item, DiagCode::D2);
        assert!(run_names(None, &deps).unwrap().items.len() == 5);
    }

    #[test]
    fn now_ms_injectable() {
        let mut deps = mock_deps();
        deps.now_ms = Box::new(|| 42);
        let report = run(Some(&[DiagCode::D3]), &deps);
        assert_eq!(report.generated_at_ms, 42);
        assert_eq!(report.items[0].timestamp_ms, 42);
    }

    /// 默认依赖构造不 panic（不发起真实网络——仅构造）。
    #[test]
    fn default_deps_construct() {
        let deps = DoctorDeps::default();
        assert!(deps.timeout > Duration::ZERO);
        let ts = (deps.now_ms)();
        assert!(ts > 0);
    }

    /// run_with_matrix：D4 使用调用方矩阵（mock 探测，离线）。
    #[test]
    fn run_with_matrix_uses_caller_targets_for_d4() {
        use std::sync::Arc;

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        struct Counting(Arc<std::sync::atomic::AtomicUsize>);
        impl HttpProbe for Counting {
            fn head(&self, _url: &str, _t: Duration) -> ProbeOutcome {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                ProbeOutcome {
                    success: true,
                    ttfb_ms: 3,
                    status: Some(200),
                    error: None,
                }
            }
        }
        let mut deps = mock_deps();
        deps.http = Box::new(Counting(calls.clone()));
        let targets = vec![ChannelTarget {
            resource: "registry JSON".into(),
            channel: aibox_net::ChannelType::Direct,
            name: "raw.github".into(),
            url: "http://raw.example/".into(),
        }];
        let report = run_with_matrix(Some(&[DiagCode::D4]), &deps, &targets);
        assert_eq!(report.items.len(), 1);
        assert_eq!(report.items[0].status, DiagStatus::Ok);
        assert_eq!(report.items[0].detail["total"], 1);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
