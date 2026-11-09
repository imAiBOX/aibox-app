//! D4 通道实测：
//! 对调用方传入的通道矩阵**并发**探测，输出测速表（通道名/状态/TTFB/当前排序）。
//! 矩阵数据由调用方（NetLayer / registry）提供，本 crate 不硬编码全矩阵；
//! 探测原语复用 `aibox-net::probe_head`（经 `HttpProbe` 注入）。

use std::thread;

use serde_json::json;

use aibox_net::ChannelType;

use crate::report::{loc, DiagCard, DiagCode, DiagStatus, Fix, FixLevel};
use crate::transport::DoctorDeps;

/// 一条待测通道（调用方从矩阵展开）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelTarget {
    /// 资源名（如 `registry JSON` / `Node.js Win`）。
    pub resource: String,
    /// 通道类型（DIRECT/MIRROR/PROXY/CUSTOM）。
    pub channel: ChannelType,
    /// 展示名（如 `jsDelivr`、`ghfast.top`）。
    pub name: String,
    /// 探测 URL。
    pub url: String,
}

/// 测速表一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRow {
    pub resource: String,
    pub channel: ChannelType,
    pub name: String,
    pub url: String,
    pub ok: bool,
    pub ttfb_ms: u64,
    pub status: Option<u16>,
    pub error: Option<String>,
    /// 当前排序（1 起；成功优先、TTFB 升序）。
    pub rank: usize,
}

/// 排序规则（纯函数）：成功 > 失败；同成败按 TTFB 升序；再按名字稳定排序。
pub fn sort_rows(rows: &mut [ChannelRow]) {
    rows.sort_by(|a, b| {
        b.ok.cmp(&a.ok)
            .then(a.ttfb_ms.cmp(&b.ttfb_ms))
            .then(a.name.cmp(&b.name))
    });
    for (i, row) in rows.iter_mut().enumerate() {
        row.rank = i + 1;
    }
}

/// 并发探测全部通道并排序（探测原语经 `DoctorDeps.http` 注入，单测离线 mock）。
pub fn probe_matrix(targets: &[ChannelTarget], deps: &DoctorDeps) -> Vec<ChannelRow> {
    let mut rows: Vec<ChannelRow> = thread::scope(|s| {
        let handles: Vec<_> = targets
            .iter()
            .cloned()
            .map(|target| {
                let http = &deps.http;
                let timeout = deps.timeout;
                let url = target.url.clone();
                let handle = s.spawn(move || http.head(&url, timeout));
                (target, handle)
            })
            .collect();

        handles
            .into_iter()
            .map(|(target, handle)| {
                let outcome = handle.join().unwrap_or_else(|_| aibox_net::ProbeOutcome {
                    success: false,
                    ttfb_ms: 0,
                    status: None,
                    error: Some("probe thread panicked".to_string()),
                });
                ChannelRow {
                    resource: target.resource,
                    channel: target.channel,
                    name: target.name,
                    url: target.url,
                    ok: outcome.success,
                    ttfb_ms: outcome.ttfb_ms,
                    status: outcome.status,
                    error: outcome.error,
                    rank: 0,
                }
            })
            .collect()
    });
    sort_rows(&mut rows);
    rows
}

/// 执行 D4。
pub fn run_with(targets: &[ChannelTarget], deps: &DoctorDeps) -> DiagCard {
    let rows = probe_matrix(targets, deps);
    let ok_count = rows.iter().filter(|r| r.ok).count();
    let total = rows.len();

    let (status, summary, summary_key) = if total == 0 {
        (
            DiagStatus::Warn,
            loc(
                "未提供通道矩阵，无法实测（由调用方传入 5.9.4 矩阵）",
                "No channel matrix provided (caller supplies the 5.9.4 matrix)",
            ),
            "netdoctor.D4.no_targets".to_string(),
        )
    } else if ok_count == total {
        (
            DiagStatus::Ok,
            loc(
                format!("全部 {total} 条通道可用"),
                format!("All {total} channels reachable"),
            ),
            "netdoctor.D4.all_ok".to_string(),
        )
    } else if ok_count == 0 {
        (
            DiagStatus::Fail,
            loc(
                format!("全部 {total} 条通道均不可达"),
                format!("All {total} channels unreachable"),
            ),
            "netdoctor.D4.all_dead".to_string(),
        )
    } else {
        (
            DiagStatus::Warn,
            loc(
                format!("{ok_count}/{total} 条通道可用（将按 TTFB 排序降级）"),
                format!("{ok_count}/{total} channels reachable (sorted by TTFB for fallback)"),
            ),
            "netdoctor.D4.partial".to_string(),
        )
    };

    let mut fixes = Vec::new();
    if ok_count < total {
        fixes.push(Fix::new(
            FixLevel::R1,
            "netdoctor.fix.channel_fallback",
            loc(
                "通道自动降级：按当前排序切换到最快可用通道（自动）",
                "Automatic channel fallback to the fastest working channel (automatic)",
            ),
            "仅影响 aibox 进程内下载选路；全挂时报 NET_ALL_CHANNELS_DEAD 并唤起 E1",
        ));
    }

    DiagCard {
        item: DiagCode::D4,
        status,
        title: DiagCode::D4.title(),
        summary_key,
        summary,
        detail: json!({
            "total": total,
            "ok": ok_count,
            "rows": rows.iter().map(|r| json!({
                "resource": r.resource,
                "channel": r.channel.as_str(),
                "name": r.name,
                "url": r.url,
                "ok": r.ok,
                "ttfb_ms": r.ttfb_ms,
                "status": r.status,
                "error": r.error,
                "rank": r.rank,
            })).collect::<Vec<_>>(),
        }),
        timestamp_ms: (deps.now_ms)(),
        fixes,
    }
}

/// 默认配置执行 D4（targets 必须由调用方传入）。
pub fn run(targets: &[ChannelTarget], deps: &DoctorDeps) -> DiagCard {
    run_with(targets, deps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::DiagStatus;
    use crate::transport::{HeadProbe, HttpProbe};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// 本地一次性 HTTP 服务器：延迟 `delay_ms` 后回 200。返回 URL 与实际端口。
    fn serve(delay_ms: u64) -> (String, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                thread::sleep(Duration::from_millis(delay_ms));
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}/"), addr.port())
    }

    /// 已释放端口（连接必拒）。
    fn dead_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);
        format!("http://{addr}/")
    }

    #[test]
    fn sort_rows_orders_success_then_ttfb() {
        let row = |name: &str, ok: bool, ttfb: u64| ChannelRow {
            resource: "res".into(),
            channel: ChannelType::Direct,
            name: name.into(),
            url: "http://x/".into(),
            ok,
            ttfb_ms: ttfb,
            status: if ok { Some(200) } else { None },
            error: if ok { None } else { Some("e".into()) },
            rank: 0,
        };
        let mut rows = vec![
            row("slow-ok", true, 300),
            row("dead", false, 5),
            row("fast-ok", true, 40),
            row("mid-ok", true, 120),
        ];
        sort_rows(&mut rows);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["fast-ok", "mid-ok", "slow-ok", "dead"]);
        let ranks: Vec<usize> = rows.iter().map(|r| r.rank).collect();
        assert_eq!(ranks, [1, 2, 3, 4]);
        // 失败行也参与排序但排最后
        assert!(!rows[3].ok);
    }

    /// D4 并发探测：本地 TcpListener mock（快/慢/死三通道）。
    #[test]
    fn d4_concurrent_probe_with_local_mock_servers() {
        let (fast_url, fast_port) = serve(50);
        let (slow_url, slow_port) = serve(250);
        let dead = dead_url();

        let targets = vec![
            ChannelTarget {
                resource: "registry JSON".into(),
                channel: ChannelType::Direct,
                name: "raw.github".into(),
                url: fast_url.clone(),
            },
            ChannelTarget {
                resource: "registry JSON".into(),
                channel: ChannelType::Mirror,
                name: "jsDelivr".into(),
                url: slow_url.clone(),
            },
            ChannelTarget {
                resource: "Node.js Win".into(),
                channel: ChannelType::Proxy,
                name: "gh-gateway".into(),
                url: dead.clone(),
            },
        ];

        let deps = DoctorDeps {
            http: Box::new(HeadProbe),
            timeout: Duration::from_secs(3),
            ..DoctorDeps::default()
        };

        let start = std::time::Instant::now();
        let card = run(&targets, &deps);
        let elapsed = start.elapsed();

        // 并发：总耗时应接近最慢一路（250ms），而非三路串行（≥300ms+）
        assert!(
            elapsed < Duration::from_millis(500),
            "probes should run concurrently, took {elapsed:?}"
        );

        assert_eq!(card.item, DiagCode::D4);
        assert_eq!(card.status, DiagStatus::Warn);
        assert_eq!(card.summary_key, "netdoctor.D4.partial");
        assert_eq!(card.detail["total"], 3);
        assert_eq!(card.detail["ok"], 2);

        let rows = card.detail["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        // 排序：成功按 TTFB（fast 在 slow 前），失败最后
        assert_eq!(rows[0]["name"], "raw.github");
        assert_eq!(rows[0]["ok"], true);
        assert_eq!(rows[0]["rank"], 1);
        assert_eq!(rows[1]["name"], "jsDelivr");
        assert_eq!(rows[2]["name"], "gh-gateway");
        assert_eq!(rows[2]["ok"], false);
        assert!(rows[2]["error"].is_string());

        // 端口确实分别在测（避免 URL 串位）
        assert!(rows[0]["url"].as_str().unwrap().contains(&fast_port.to_string()));
        assert!(rows[1]["url"].as_str().unwrap().contains(&slow_port.to_string()));

        // 降级建议 = R1 自动
        assert_eq!(card.fixes.len(), 1);
        assert_eq!(card.fixes[0].level, FixLevel::R1);
        assert!(card.fixes[0].level.is_auto());
    }

    #[test]
    fn d4_all_ok_and_all_dead() {
        let (url, _) = serve(10);
        let deps = DoctorDeps {
            http: Box::new(HeadProbe),
            timeout: Duration::from_secs(3),
            ..DoctorDeps::default()
        };
        let targets = vec![ChannelTarget {
            resource: "PyPI".into(),
            channel: ChannelType::Direct,
            name: "pypi.org".into(),
            url: url.clone(),
        }];
        let card = run(&targets, &deps);
        assert_eq!(card.status, DiagStatus::Ok);
        assert_eq!(card.summary_key, "netdoctor.D4.all_ok");
        assert!(card.fixes.is_empty());

        let dead = vec![ChannelTarget {
            resource: "PyPI".into(),
            channel: ChannelType::Direct,
            name: "pypi.org".into(),
            url: dead_url(),
        }];
        let card2 = run(&dead, &deps);
        assert_eq!(card2.status, DiagStatus::Fail);
        assert_eq!(card2.summary_key, "netdoctor.D4.all_dead");
        assert_eq!(card2.fixes[0].level, FixLevel::R1);
    }

    #[test]
    fn d4_empty_targets_is_warn() {
        let deps = DoctorDeps::default();
        let card = run(&[], &deps);
        assert_eq!(card.status, DiagStatus::Warn);
        assert_eq!(card.summary_key, "netdoctor.D4.no_targets");
        assert!(card.fixes.is_empty());
    }

    /// mock 探测原语：验证「矩阵由调用方传入、探测可注入」且不依赖真实网络。
    struct CountingProbe {
        calls: std::sync::Arc<AtomicUsize>,
    }
    impl HttpProbe for CountingProbe {
        fn head(&self, url: &str, _t: Duration) -> aibox_net::ProbeOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            aibox_net::ProbeOutcome {
                success: !url.contains("dead"),
                ttfb_ms: if url.contains("fast") { 10 } else { 100 },
                status: Some(200),
                error: None,
            }
        }
    }

    #[test]
    fn d4_matrix_is_caller_supplied_and_probe_injectable() {
        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let probe = CountingProbe {
            calls: calls.clone(),
        };
        let deps = DoctorDeps {
            http: Box::new(probe),
            ..DoctorDeps::default()
        };
        // 调用方自定义矩阵（netdoctor 不硬编码）
        let targets = vec![
            ChannelTarget {
                resource: "custom".into(),
                channel: ChannelType::Custom,
                name: "my-fast".into(),
                url: "http://fast/".into(),
            },
            ChannelTarget {
                resource: "custom".into(),
                channel: ChannelType::Custom,
                name: "my-dead".into(),
                url: "http://dead/".into(),
            },
        ];
        let card = run(&targets, &deps);
        assert_eq!(card.detail["ok"], 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let rows = card.detail["rows"].as_array().unwrap();
        assert_eq!(rows[0]["name"], "my-fast");
        assert_eq!(rows[1]["ok"], false);
    }
}
