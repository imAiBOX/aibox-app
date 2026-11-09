//! 通道探测与评分（ProbeScore，会话级缓存 15 分钟）。
//!
//! - 记分：`success × 1000 / max(TTFB_ms, 1)`；失败 = 0 分。
//! - 失效与复活：连续 2 次失败 → 本会话剔除；15 分钟后复活重测。
//! - 缓存：探测结果 15 分钟内复用（`probe_with_cache` 不重复发请求）。
//!
//! 全部时间语义走注入的 [`Clock`]，离线可测。

use std::collections::HashMap;
use std::time::Duration;

use crate::channel::ChannelType;
use crate::clock::{system_clock, SharedClock};
use crate::probe::{probe_head, ProbeOutcome};
use crate::region::{order_channels, RegionKind};

/// 探测结果会话级缓存 TTL（15 分钟）。
pub const SCORE_TTL_MS: u64 = 15 * 60 * 1000;
/// 连续失败剔除阈值。
pub const FAIL_STRIKE_LIMIT: u32 = 2;
/// 剔除后的复活延迟（15 分钟）。
pub const REVIVE_AFTER_MS: u64 = 15 * 60 * 1000;

/// 一条通道的评分记录。
#[derive(Debug, Clone)]
pub struct ProbeRecord {
    pub outcome: ProbeOutcome,
    /// `success ? 1000 / max(ttfb_ms, 1) : 0`
    pub score: f64,
    /// 记录时刻（Clock 毫秒）
    pub at_ms: u64,
    /// 连续失败次数（成功清零）
    pub consecutive_failures: u32,
    /// 剔除截止时刻（None = 未剔除）
    pub removed_until_ms: Option<u64>,
}

impl ProbeRecord {
    fn is_fresh(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.at_ms) < SCORE_TTL_MS
    }

    fn is_removed(&self, now_ms: u64) -> bool {
        match self.removed_until_ms {
            Some(until) => now_ms < until,
            None => false,
        }
    }
}

/// 会话级评分存储：评分 / 缓存 / 剔除与复活。
#[derive(Debug)]
pub struct ProbeScore {
    map: HashMap<ChannelType, ProbeRecord>,
    clock: SharedClock,
}

impl ProbeScore {
    pub fn new(clock: SharedClock) -> Self {
        Self {
            map: HashMap::new(),
            clock,
        }
    }

    /// 使用进程真实时钟。
    pub fn with_system_clock() -> Self {
        Self::new(system_clock())
    }

    /// 记分公式（纯函数）：`success × 1000 / max(ttfb_ms, 1)`。
    pub fn score_of(outcome: &ProbeOutcome) -> f64 {
        if outcome.success {
            1000.0 / (outcome.ttfb_ms.max(1) as f64)
        } else {
            0.0
        }
    }

    /// 记录一次探测结果（返回新记录）。
    ///
    /// - 成功：清零连续失败、按公式记分；
    /// - 失败：0 分、连续失败 +1，达到 `FAIL_STRIKE_LIMIT` → 剔除 15 分钟；
    /// - 已到复活时刻的剔除记录：重新给满失败机会（复活重测）。
    pub fn record(&mut self, channel: ChannelType, outcome: ProbeOutcome) -> ProbeRecord {
        let now = self.clock.now_ms();
        let score = Self::score_of(&outcome);
        let mut consecutive_failures = 0u32;
        if !outcome.success {
            let revived = self
                .map
                .get(&channel)
                .map(|prev| {
                    prev.removed_until_ms
                        .map(|until| now >= until)
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            let prev_failures = self
                .map
                .get(&channel)
                .map(|p| p.consecutive_failures)
                .unwrap_or(0);
            consecutive_failures = if revived { 0 } else { prev_failures } + 1;
        }
        let removed_until_ms = if !outcome.success && consecutive_failures >= FAIL_STRIKE_LIMIT {
            Some(now + REVIVE_AFTER_MS)
        } else {
            None
        };
        let record = ProbeRecord {
            outcome,
            score,
            at_ms: now,
            consecutive_failures,
            removed_until_ms,
        };
        self.map.insert(channel, record.clone());
        record
    }

    /// 取 15 分钟缓存内的记录（过期 → None，需要重测）。
    pub fn get_fresh(&self, channel: ChannelType) -> Option<&ProbeRecord> {
        let now = self.clock.now_ms();
        self.map.get(&channel).filter(|r| r.is_fresh(now))
    }

    /// 通道是否参与本轮探测/下载（未剔除，或已到复活时刻）。
    pub fn is_eligible(&self, channel: ChannelType) -> bool {
        let now = self.clock.now_ms();
        match self.map.get(&channel) {
            Some(r) => !r.is_removed(now),
            None => true,
        }
    }

    /// 通道当前是否处于剔除期。
    pub fn is_removed(&self, channel: ChannelType) -> bool {
        let now = self.clock.now_ms();
        self.map.get(&channel).map(|r| r.is_removed(now)).unwrap_or(false)
    }

    /// 无缓存时的静默探测（离线测试走 `record`/`get_fresh`）。
    pub fn probe(&mut self, channel: ChannelType, url: &str, timeout: Duration) -> ProbeOutcome {
        let outcome = probe_head(url, timeout);
        self.record(channel, outcome.clone());
        outcome
    }

    /// 15 分钟缓存探测：缓存命中直接返回，不发网络请求。
    pub fn probe_with_cache(
        &mut self,
        channel: ChannelType,
        url: &str,
        timeout: Duration,
    ) -> ProbeOutcome {
        if let Some(rec) = self.get_fresh(channel) {
            return rec.outcome.clone();
        }
        self.probe(channel, url, timeout)
    }

    /// 排序候选通道：先按区域静态序，再按新鲜评分降序（稳定排序，
    /// 同分保持区域序）；剔除中的通道不参与。
    pub fn ranked(&self, kind: &[ChannelType], region: RegionKind) -> Vec<ChannelType> {
        let now = self.clock.now_ms();
        let mut candidates: Vec<ChannelType> = order_channels(kind, region)
            .into_iter()
            .filter(|c| self.map.get(c).map(|r| !r.is_removed(now)).unwrap_or(true))
            .collect();
        candidates.sort_by(|a, b| {
            let sa = self.map.get(a).filter(|r| r.is_fresh(now)).map(|r| r.score).unwrap_or(0.0);
            let sb = self.map.get(b).filter(|r| r.is_fresh(now)).map(|r| r.score).unwrap_or(0.0);
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });
        candidates
    }

    /// 当前最优通道（无候选 → None）。
    pub fn best(&self, kind: &[ChannelType], region: RegionKind) -> Option<ChannelType> {
        self.ranked(kind, region).into_iter().next()
    }

    /// 已记录通道数。
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use std::sync::Arc;

    fn ok(ttfb: u64) -> ProbeOutcome {
        ProbeOutcome {
            success: true,
            ttfb_ms: ttfb,
            status: Some(200),
            error: None,
        }
    }

    fn fail(msg: &str) -> ProbeOutcome {
        ProbeOutcome {
            success: false,
            ttfb_ms: 5000,
            status: None,
            error: Some(msg.to_string()),
        }
    }

    fn setup() -> (ProbeScore, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new());
        let score = ProbeScore::new(Arc::clone(&clock) as SharedClock);
        (score, clock)
    }

    #[test]
    fn score_formula_matches_spec() {
        assert_eq!(ProbeScore::score_of(&ok(1)), 1000.0);
        assert_eq!(ProbeScore::score_of(&ok(0)), 1000.0); // max(0,1)=1
        assert_eq!(ProbeScore::score_of(&ok(1000)), 1.0);
        assert_eq!(ProbeScore::score_of(&ok(400)), 2.5);
        assert_eq!(ProbeScore::score_of(&fail("x")), 0.0);
    }

    #[test]
    fn two_consecutive_failures_remove_then_revive_after_15min() {
        let (mut score, clock) = setup();
        let ch = ChannelType::Mirror;

        score.record(ch, fail("e1"));
        assert!(!score.is_removed(ch), "1 次失败不剔除");
        assert!(score.is_eligible(ch));

        score.record(ch, fail("e2"));
        assert!(score.is_removed(ch), "连续 2 次失败 → 剔除");
        assert!(!score.is_eligible(ch));

        // 14:59 仍在剔除期
        clock.advance_ms(REVIVE_AFTER_MS - 1);
        assert!(!score.is_eligible(ch));
        // 15:00 复活（重测资格恢复）
        clock.advance_ms(1);
        assert!(score.is_eligible(ch), "15 分钟后复活");
        assert!(!score.is_removed(ch));

        // 复活后再失败 → 重新剔除（复活给满 2 次机会：1 失败不剔）
        score.record(ch, fail("e3"));
        assert!(!score.is_removed(ch), "复活后第 1 次失败重新计数");
        score.record(ch, fail("e4"));
        assert!(score.is_removed(ch), "复活后连续 2 次失败再次剔除");
    }

    #[test]
    fn success_resets_failure_streak() {
        let (mut score, _clock) = setup();
        let ch = ChannelType::Proxy;
        score.record(ch, fail("e1"));
        score.record(ch, fail("e2"));
        assert!(score.is_removed(ch));
        // 成功（含在剔除期内主动复测）清零
        clock_advance_success(&mut score, ch);
        assert!(!score.is_removed(ch), "成功后不再剔除");
        assert_eq!(score.get_fresh(ch).unwrap().consecutive_failures, 0);
        assert!(score.get_fresh(ch).unwrap().score > 0.0);
    }

    fn clock_advance_success(score: &mut ProbeScore, ch: ChannelType) {
        score.record(ch, ok(100));
    }

    #[test]
    fn cache_ttl_15_minutes() {
        let (mut score, clock) = setup();
        let ch = ChannelType::Direct;
        score.record(ch, ok(50));

        assert!(score.get_fresh(ch).is_some());
        clock.advance_ms(SCORE_TTL_MS - 1);
        assert!(score.get_fresh(ch).is_some(), "14:59 仍新鲜");
        clock.advance_ms(1);
        assert!(score.get_fresh(ch).is_none(), "15:00 过期需重测");
        // 过期不影响剔除状态判定
        assert!(score.is_eligible(ch));
    }

    #[test]
    fn probe_with_cache_hits_without_network() {
        let (mut score, _clock) = setup();
        // 先手动记录一条成功（缓存有效期内）
        score.record(ChannelType::Mirror, ok(120));
        // 再用必然连不上的 URL 探测：若发了网络请求必失败，命中缓存则返回旧成功
        let outcome = score.probe_with_cache(
            ChannelType::Mirror,
            "http://127.0.0.1:1/unreachable",
            Duration::from_millis(300),
        );
        assert!(outcome.success, "缓存命中：不发网络请求");
        assert_eq!(outcome.ttfb_ms, 120);
        assert_eq!(score.len(), 1, "没有产生新记录");
    }

    #[test]
    fn ranked_orders_by_score_then_region_and_drops_removed() {
        let (mut score, _clock) = setup();
        // CN 序下：DIRECT 实测更快 → 排到 MIRROR 前
        score.record(ChannelType::Direct, ok(10)); // score 100
        score.record(ChannelType::Mirror, ok(500)); // score 2
        score.record(ChannelType::Proxy, ok(200)); // score 5
        let all = ChannelType::all();
        let ranked = score.ranked(&all, RegionKind::Cn);
        assert_eq!(
            ranked,
            vec![
                ChannelType::Direct,
                ChannelType::Proxy,
                ChannelType::Mirror,
                ChannelType::Custom
            ],
            "新鲜评分降序；无数据的 CUSTOM 保持区域序末位"
        );

        // 剔除中的通道不参与
        score.record(ChannelType::Direct, fail("e1"));
        score.record(ChannelType::Direct, fail("e2"));
        let ranked = score.ranked(&all, RegionKind::Cn);
        assert!(!ranked.contains(&ChannelType::Direct), "剔除中的通道出局");
        assert_eq!(ranked[0], ChannelType::Proxy);
        assert_eq!(score.best(&all, RegionKind::Cn), Some(ChannelType::Proxy));
    }

    #[test]
    fn ranked_without_data_keeps_region_order() {
        let (score, _clock) = setup();
        assert_eq!(
            score.ranked(&ChannelType::all(), RegionKind::Cn),
            order_channels(&ChannelType::all(), RegionKind::Cn)
        );
    }

    #[test]
    fn stale_scores_treated_as_zero_ties() {
        let (mut score, clock) = setup();
        score.record(ChannelType::Direct, ok(10)); // 高分但将过期
        score.record(ChannelType::Mirror, ok(1000)); // 低分
        clock.advance_ms(SCORE_TTL_MS); // 全部过期
        // 过期 → 都按 0 分 → 稳定排序回到 CN 区域序
        assert_eq!(
            score.ranked(&ChannelType::all(), RegionKind::Cn),
            order_channels(&ChannelType::all(), RegionKind::Cn)
        );
    }
}
