//! D0 环境锚测：
//! baidu.com 与 github.com 各发 204 探测 → 分类 纯国内 / 国际 / 疑似代理中转。
//!
//! 「疑似代理中转」判定为**阈值函数**（纯函数、可单测）：两者皆通且
//! github 延迟「反常地快且稳定」（国内直连 github 通常 150-400ms 且抖动大；
//! 经加速器/代理中转则 github 往往 <80ms 且离散极小）。

use std::thread;

use aibox_net::ProbeOutcome;
use serde_json::json;

use crate::report::{loc, DiagCard, DiagCode, DiagStatus};
use crate::transport::DoctorDeps;

/// 锚测端点：百度 `generate_204`（返回 204）与 GitHub 首页。
pub const D0_BAIDU_URL: &str = "https://www.baidu.com/generate_204";
pub const D0_GITHUB_URL: &str = "https://github.com/";

/// 阈值（「github 延迟规律异常」的可调参数）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct D0Thresholds {
    /// github TTFB 中位数上限（ms）：低于此值视为「快得反常」。
    pub github_fast_max_ms: u64,
    /// baidu TTFB 中位数上限（ms）：baidu 也快说明整体链路在国内/低延迟，
    /// 排除「海外直连（github 快但 baidu 慢）」的误报。
    pub baidu_fast_max_ms: u64,
    /// github 样本离散度（max-min）上限（ms）：中转链路延迟稳定、离散小。
    pub max_spread_ms: u64,
    /// 判定异常所需 github 最少成功样本数。
    pub min_samples: usize,
}

impl Default for D0Thresholds {
    fn default() -> Self {
        D0Thresholds {
            github_fast_max_ms: 80,
            baidu_fast_max_ms: 100,
            max_spread_ms: 20,
            min_samples: 2,
        }
    }
}

/// 环境分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvClass {
    /// 纯国内：baidu 通、github 不通。
    CnOnly,
    /// 国际：github 可达且无中转特征。
    International,
    /// 疑似代理/加速器中转：两者皆通但 github 延迟规律异常（快且稳）。
    SuspiciousRelay,
    /// 双双不可达：断网或必须走代理。
    Unreachable,
}

impl EnvClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            EnvClass::CnOnly => "cn_only",
            EnvClass::International => "international",
            EnvClass::SuspiciousRelay => "suspicious_relay",
            EnvClass::Unreachable => "unreachable",
        }
    }

    pub fn status(&self) -> DiagStatus {
        match self {
            EnvClass::CnOnly | EnvClass::International => DiagStatus::Ok,
            EnvClass::SuspiciousRelay => DiagStatus::Warn,
            EnvClass::Unreachable => DiagStatus::Fail,
        }
    }

    pub fn summary(&self) -> crate::report::Localized {
        match self {
            EnvClass::CnOnly => loc(
                "纯国内网络：百度可达，GitHub 不可达（正常，走国内镜像路线）",
                "Purely domestic network: Baidu reachable, GitHub unreachable (use CN mirrors)",
            ),
            EnvClass::International => loc(
                "国际网络：GitHub 与国内站点均可达",
                "International network: both GitHub and domestic sites reachable",
            ),
            EnvClass::SuspiciousRelay => loc(
                "疑似经代理/加速器中转：GitHub 延迟异常地快且稳定",
                "Possibly relayed via proxy/accelerator: GitHub latency unusually fast and stable",
            ),
            EnvClass::Unreachable => loc(
                "无法访问百度与 GitHub：网络可能断开，或必须经代理才能上网",
                "Neither Baidu nor GitHub reachable: offline, or a proxy is required",
            ),
        }
    }
}

/// 单个锚点的样本统计。
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorStats {
    pub ok: usize,
    pub total: usize,
    pub ttfs: Vec<u64>,
}

impl AnchorStats {
    pub fn from_outcomes(samples: &[ProbeOutcome]) -> Self {
        let mut ttfs: Vec<u64> = samples
            .iter()
            .filter(|o| o.success)
            .map(|o| o.ttfb_ms)
            .collect();
        ttfs.sort_unstable();
        AnchorStats {
            ok: ttfs.len(),
            total: samples.len(),
            ttfs,
        }
    }

    /// 中位数（偶数个取中间两值均值）。
    pub fn median(&self) -> Option<u64> {
        let n = self.ttfs.len();
        if n == 0 {
            return None;
        }
        let mid = n / 2;
        Some(if n % 2 == 1 {
            self.ttfs[mid]
        } else {
            (self.ttfs[mid - 1] + self.ttfs[mid]) / 2
        })
    }

    /// 离散度 max-min。
    pub fn spread(&self) -> Option<u64> {
        match (self.ttfs.first(), self.ttfs.last()) {
            (Some(min), Some(max)) => Some(max - min),
            _ => None,
        }
    }
}

/// D0 判定阈值函数（纯函数）：给定两个锚点的探测样本 → 环境分类。
pub fn classify_env(
    baidu: &[ProbeOutcome],
    github: &[ProbeOutcome],
    th: &D0Thresholds,
) -> EnvClass {
    let b = AnchorStats::from_outcomes(baidu);
    let g = AnchorStats::from_outcomes(github);

    if b.ok == 0 && g.ok == 0 {
        return EnvClass::Unreachable;
    }
    if g.ok == 0 {
        return EnvClass::CnOnly;
    }
    if b.ok == 0 {
        return EnvClass::International;
    }
    // 两者皆通：github 是否「反常地快且稳」→ 疑似中转
    let (Some(g_med), Some(g_spread), Some(b_med)) = (g.median(), g.spread(), b.median())
    else {
        return EnvClass::International;
    };
    if g.ok >= th.min_samples
        && g_med <= th.github_fast_max_ms
        && b_med <= th.baidu_fast_max_ms
        && g_spread <= th.max_spread_ms
    {
        EnvClass::SuspiciousRelay
    } else {
        EnvClass::International
    }
}

/// D0 配置（URL/轮数/阈值均可注入）。
#[derive(Debug, Clone)]
pub struct D0Config {
    pub baidu_url: String,
    pub github_url: String,
    /// 采样轮数（每轮两锚点并发探测）。
    pub rounds: usize,
    pub thresholds: D0Thresholds,
}

impl Default for D0Config {
    fn default() -> Self {
        D0Config {
            baidu_url: D0_BAIDU_URL.to_string(),
            github_url: D0_GITHUB_URL.to_string(),
            rounds: 3,
            thresholds: D0Thresholds::default(),
        }
    }
}

fn thread_panic_outcome() -> ProbeOutcome {
    ProbeOutcome {
        success: false,
        ttfb_ms: 0,
        status: None,
        error: Some("probe thread panicked".to_string()),
    }
}

/// 执行 D0，产出体检卡。
pub fn run_with(deps: &DoctorDeps, cfg: &D0Config) -> DiagCard {
    let rounds = cfg.rounds.max(1);
    let mut baidu: Vec<ProbeOutcome> = Vec::with_capacity(rounds);
    let mut github: Vec<ProbeOutcome> = Vec::with_capacity(rounds);

    for _ in 0..rounds {
        // 两锚点并发探测（不互相拖慢）
        let (b, g) = thread::scope(|s| {
            let hb = s.spawn(|| deps.http.head(&cfg.baidu_url, deps.timeout));
            let hg = s.spawn(|| deps.http.head(&cfg.github_url, deps.timeout));
            (
                hb.join().unwrap_or_else(|_| thread_panic_outcome()),
                hg.join().unwrap_or_else(|_| thread_panic_outcome()),
            )
        });
        baidu.push(b);
        github.push(g);
    }

    let class = classify_env(&baidu, &github, &cfg.thresholds);
    let b_stats = AnchorStats::from_outcomes(&baidu);
    let g_stats = AnchorStats::from_outcomes(&github);

    DiagCard {
        item: DiagCode::D0,
        status: class.status(),
        title: DiagCode::D0.title(),
        summary_key: format!("netdoctor.D0.{}", class.as_str()),
        summary: class.summary(),
        detail: json!({
            "class": class.as_str(),
            "rounds": rounds,
            "baidu": {
                "url": cfg.baidu_url,
                "ok": b_stats.ok,
                "total": b_stats.total,
                "median_ttfb_ms": b_stats.median(),
                "spread_ms": b_stats.spread(),
            },
            "github": {
                "url": cfg.github_url,
                "ok": g_stats.ok,
                "total": g_stats.total,
                "median_ttfb_ms": g_stats.median(),
                "spread_ms": g_stats.spread(),
            },
            "thresholds": {
                "github_fast_max_ms": cfg.thresholds.github_fast_max_ms,
                "baidu_fast_max_ms": cfg.thresholds.baidu_fast_max_ms,
                "max_spread_ms": cfg.thresholds.max_spread_ms,
                "min_samples": cfg.thresholds.min_samples,
            },
        }),
        timestamp_ms: (deps.now_ms)(),
        fixes: vec![],
    }
}

/// 默认配置执行 D0。
pub fn run(deps: &DoctorDeps) -> DiagCard {
    run_with(deps, &D0Config::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::DiagStatus;
    use crate::transport::HttpProbe;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn ok(ttfb: u64) -> ProbeOutcome {
        ProbeOutcome {
            success: true,
            ttfb_ms: ttfb,
            status: Some(204),
            error: None,
        }
    }

    fn fail() -> ProbeOutcome {
        ProbeOutcome {
            success: false,
            ttfb_ms: 0,
            status: None,
            error: Some("timeout".into()),
        }
    }

    /// 按 URL 返回预设序列的 mock 探测（序列取尽后重复最后一个）。
    struct SeqProbe {
        baidu: Vec<ProbeOutcome>,
        github: Vec<ProbeOutcome>,
        calls: AtomicUsize,
    }

    impl HttpProbe for SeqProbe {
        fn head(&self, url: &str, _t: Duration) -> ProbeOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let seq = if url.contains("baidu") {
                &self.baidu
            } else {
                &self.github
            };
            let idx = (self.calls.load(Ordering::SeqCst) - 1) % seq.len().max(1);
            seq.get(idx).cloned().unwrap_or_else(fail)
        }
    }

    #[test]
    fn classify_cn_only() {
        let th = D0Thresholds::default();
        let baidu = vec![ok(30), ok(32), ok(28)];
        let github = vec![fail(), fail(), fail()];
        assert_eq!(classify_env(&baidu, &github, &th), EnvClass::CnOnly);
        assert_eq!(
            classify_env(&baidu, &github, &th).status(),
            DiagStatus::Ok
        );
    }

    #[test]
    fn classify_unreachable() {
        let th = D0Thresholds::default();
        let github = vec![fail(), fail()];
        assert_eq!(
            classify_env(&[fail(), fail()], &github, &th),
            EnvClass::Unreachable
        );
        assert_eq!(
            classify_env(&[fail(), fail()], &github, &th).status(),
            DiagStatus::Fail
        );
    }

    #[test]
    fn classify_international_when_github_slow_or_jittery() {
        let th = D0Thresholds::default();
        // github 慢（直连国际链路典型 200-400ms）
        let baidu = vec![ok(40), ok(45), ok(42)];
        let github = vec![ok(260), ok(310), ok(290)];
        assert_eq!(classify_env(&baidu, &github, &th), EnvClass::International);

        // github 快但抖动大（spread > 20ms）→ 不判中转
        let github_jitter = vec![ok(20), ok(120), ok(60)];
        assert_eq!(
            classify_env(&baidu, &github_jitter, &th),
            EnvClass::International
        );

        // github 快且稳，但 baidu 慢（海外直连：baidu 中位数 > 100ms）→ 不误报
        let baidu_slow = vec![ok(220), ok(240), ok(230)];
        let github_fast = vec![ok(12), ok(14), ok(13)];
        assert_eq!(
            classify_env(&baidu_slow, &github_fast, &th),
            EnvClass::International
        );
    }

    #[test]
    fn classify_suspicious_relay_when_github_fast_and_stable() {
        let th = D0Thresholds::default();
        // 两者皆通、github 反常快且稳（加速器/代理中转特征）
        let baidu = vec![ok(25), ok(28), ok(26)];
        let github = vec![ok(30), ok(33), ok(31)];
        assert_eq!(
            classify_env(&baidu, &github, &th),
            EnvClass::SuspiciousRelay
        );
        assert_eq!(
            classify_env(&baidu, &github, &th).status(),
            DiagStatus::Warn
        );

        // 样本不足 min_samples → 保守判国际
        let th2 = D0Thresholds {
            min_samples: 3,
            ..D0Thresholds::default()
        };
        assert_eq!(
            classify_env(&baidu, &[ok(30)], &th2),
            EnvClass::International
        );
    }

    #[test]
    fn anchor_stats_median_and_spread() {
        let stats = AnchorStats::from_outcomes(&[ok(10), ok(30), fail(), ok(20)]);
        assert_eq!(stats.ok, 3);
        assert_eq!(stats.total, 4);
        assert_eq!(stats.median(), Some(20));
        assert_eq!(stats.spread(), Some(20));

        let empty = AnchorStats::from_outcomes(&[fail()]);
        assert_eq!(empty.median(), None);
        assert_eq!(empty.spread(), None);

        let even = AnchorStats::from_outcomes(&[ok(10), ok(20)]);
        assert_eq!(even.median(), Some(15));
    }

    #[test]
    fn d0_run_with_mock_probe() {
        let deps = DoctorDeps {
            http: Box::new(SeqProbe {
                baidu: vec![ok(25), ok(27), ok(26)],
                github: vec![ok(30), ok(32), ok(31)],
                calls: AtomicUsize::new(0),
            }),
            timeout: Duration::from_millis(50),
            ..DoctorDeps::default()
        };
        let card = run(&deps);
        assert_eq!(card.item, DiagCode::D0);
        assert_eq!(card.status, DiagStatus::Warn);
        assert_eq!(card.summary_key, "netdoctor.D0.suspicious_relay");
        assert!(card.timestamp_ms > 0);
        assert_eq!(card.detail["class"], "suspicious_relay");
        assert_eq!(card.detail["rounds"], 3);
        assert_eq!(card.detail["baidu"]["ok"], 3);
        assert_eq!(card.detail["github"]["ok"], 3);
    }
}
