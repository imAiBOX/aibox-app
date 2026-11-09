//! 报告结构：体检卡列表 + 修复建议 R1/R2/R3 分级。
//!
//! 红线：R1 = 进程内自动自愈；R2 = 用户确认后一键修复；R3 = 需管理员
//! 授权。本 crate 只产出**分级建议**，绝不修改任何系统设置（hosts/注册表/环境变量）。

use serde::{Deserialize, Serialize};

/// 人话文案（i18n key 由调用方持有；本结构提供中英兜底原文，TS 侧优先用 key）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Localized {
    pub zh: String,
    pub en: String,
}

/// 便捷构造中英文案。
pub fn loc(zh: impl Into<String>, en: impl Into<String>) -> Localized {
    Localized {
        zh: zh.into(),
        en: en.into(),
    }
}

/// 诊断项编号（D0-D4；D5/D6 属）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DiagCode {
    D0,
    D1,
    D2,
    D3,
    D4,
}

impl DiagCode {
    pub fn all() -> [DiagCode; 5] {
        [DiagCode::D0, DiagCode::D1, DiagCode::D2, DiagCode::D3, DiagCode::D4]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            DiagCode::D0 => "D0",
            DiagCode::D1 => "D1",
            DiagCode::D2 => "D2",
            DiagCode::D3 => "D3",
            DiagCode::D4 => "D4",
        }
    }

    /// 解析 `D0`/`d0`（TS↔Rust 边界用；非法值返回 None）。
    pub fn parse(s: &str) -> Option<DiagCode> {
        match s.trim().to_ascii_uppercase().as_str() {
            "D0" => Some(DiagCode::D0),
            "D1" => Some(DiagCode::D1),
            "D2" => Some(DiagCode::D2),
            "D3" => Some(DiagCode::D3),
            "D4" => Some(DiagCode::D4),
            _ => None,
        }
    }

    /// 规范顺序（D0→D4），报告输出按此排序。
    pub fn order(&self) -> usize {
        match self {
            DiagCode::D0 => 0,
            DiagCode::D1 => 1,
            DiagCode::D2 => 2,
            DiagCode::D3 => 3,
            DiagCode::D4 => 4,
        }
    }

    pub fn title_key(&self) -> String {
        format!("netdoctor.item.{}.title", self.as_str())
    }

    pub fn title(&self) -> Localized {
        match self {
            DiagCode::D0 => loc("环境锚测", "Environment anchor probe"),
            DiagCode::D1 => loc("系统代理审计", "System proxy audit"),
            DiagCode::D2 => loc("DNS 健康检测", "DNS health check"),
            DiagCode::D3 => loc("hosts 审计", "Hosts file audit"),
            DiagCode::D4 => loc("通道实测", "Channel speed test"),
        }
    }
}

/// 体检卡状态（ok/warn/fail）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiagStatus {
    Ok,
    Warn,
    Fail,
}

impl DiagStatus {
    /// 严重度：Fail > Warn > Ok（用于汇总最差状态）。
    pub fn rank(&self) -> u8 {
        match self {
            DiagStatus::Ok => 0,
            DiagStatus::Warn => 1,
            DiagStatus::Fail => 2,
        }
    }

    pub fn worse(a: DiagStatus, b: DiagStatus) -> DiagStatus {
        if a.rank() >= b.rank() {
            a
        } else {
            b
        }
    }
}

/// 修复分级。
/// - R1：进程内自愈，自动、零感知、零系统改动（主流程只允许依赖 R1）；
/// - R2：用户点确认 + 快照备份的一键修复；
/// - R3：需管理员/双层确认的显式授权修复。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FixLevel {
    R1,
    R2,
    R3,
}

impl FixLevel {
    /// TS 侧交互语义：auto=自动生效 / suggest=需用户确认 / authorize=需显式授权。
    pub fn mode(&self) -> &'static str {
        match self {
            FixLevel::R1 => "auto",
            FixLevel::R2 => "suggest",
            FixLevel::R3 => "authorize",
        }
    }

    pub fn is_auto(&self) -> bool {
        matches!(self, FixLevel::R1)
    }
}

/// 一条修复建议引用（报告只引用，不执行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fix {
    pub level: FixLevel,
    /// i18n key，例如 `netdoctor.fix.clear_env_proxy`
    pub action_key: String,
    pub summary: Localized,
    /// 实施细节/影响范围（如备份路径约定、注册表位置）。
    pub detail: String,
}

impl Fix {
    pub fn new(level: FixLevel, action_key: impl Into<String>, summary: Localized, detail: impl Into<String>) -> Self {
        Fix {
            level,
            action_key: action_key.into(),
            summary,
            detail: detail.into(),
        }
    }
}

/// 单张体检卡（D0-D4 之一）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagCard {
    pub item: DiagCode,
    pub status: DiagStatus,
    pub title: Localized,
    /// i18n key，例如 `netdoctor.D0.suspicious_relay`；summary 为兜底人话。
    pub summary_key: String,
    pub summary: Localized,
    /// 结构化明细（各诊断项自定义字段；序列化为 JSON 直传 TS）。
    pub detail: serde_json::Value,
    /// 生成时刻（Unix epoch ms）。
    pub timestamp_ms: u64,
    /// 修复建议引用（R1/R2/R3 分级；可为空）。
    pub fixes: Vec<Fix>,
}

/// 完整体检报告（`netdoctor_run` 的返回体）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagReport {
    pub schema: String,
    pub generated_at_ms: u64,
    /// 按 D0→D4 规范顺序排列。
    pub items: Vec<DiagCard>,
}

impl DiagReport {
    pub fn new(items: Vec<DiagCard>, generated_at_ms: u64) -> Self {
        let mut items = items;
        items.sort_by_key(|c| c.item.order());
        DiagReport {
            schema: "aibox-netdoctor/v1".to_string(),
            generated_at_ms,
            items,
        }
    }

    pub fn get(&self, code: DiagCode) -> Option<&DiagCard> {
        self.items.iter().find(|c| c.item == code)
    }

    /// 全报告最差状态（供 UI 顶部横幅/入口 E1 判断）。
    pub fn worst(&self) -> Option<DiagStatus> {
        self.items
            .iter()
            .map(|c| c.status)
            .reduce(DiagStatus::worse)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_card(item: DiagCode, status: DiagStatus) -> DiagCard {
        DiagCard {
            item,
            status,
            title: item.title(),
            summary_key: format!("netdoctor.{}.sample", item.as_str()),
            summary: loc("样例", "sample"),
            detail: json!({"x": 1}),
            timestamp_ms: 1_700_000_000_000,
            fixes: vec![Fix::new(
                FixLevel::R1,
                "netdoctor.fix.sample",
                loc("自动", "auto"),
                "detail",
            )],
        }
    }

    #[test]
    fn diag_code_parse_and_order() {
        assert_eq!(DiagCode::parse("d3"), Some(DiagCode::D3));
        assert_eq!(DiagCode::parse(" D0 "), Some(DiagCode::D0));
        assert_eq!(DiagCode::parse("D9"), None);
        assert_eq!(DiagCode::parse("nope"), None);
        let mut codes = DiagCode::all();
        codes.sort_by_key(|c| c.order());
        assert_eq!(
            codes.map(|c| c.as_str()),
            ["D0", "D1", "D2", "D3", "D4"]
        );
    }

    #[test]
    fn report_serializes_for_ts_boundary() {
        let report = DiagReport::new(
            vec![sample_card(DiagCode::D4, DiagStatus::Warn), sample_card(DiagCode::D0, DiagStatus::Ok)],
            42,
        );
        // 输出顺序规范：D0 在前
        assert_eq!(report.items[0].item, DiagCode::D0);
        assert_eq!(report.items[1].item, DiagCode::D4);

        let v = serde_json::to_value(&report).expect("serialize");
        assert_eq!(v["schema"], "aibox-netdoctor/v1");
        assert_eq!(v["generated_at_ms"], 42);
        assert_eq!(v["items"][0]["status"], "ok");
        assert_eq!(v["items"][1]["status"], "warn");
        assert_eq!(v["items"][0]["fixes"][0]["level"], "R1");
        assert_eq!(v["items"][0]["fixes"][0]["mode"], json!(null)); // mode 是方法非字段
        assert!(v["items"][0]["summary"]["zh"].is_string());

        // 往返
        let back: DiagReport = serde_json::from_value(v).expect("deserialize");
        assert_eq!(back.items.len(), 2);
        assert_eq!(back.items[0].item, DiagCode::D0);
    }

    #[test]
    fn worst_status_reduces() {
        let report = DiagReport::new(
            vec![
                sample_card(DiagCode::D0, DiagStatus::Ok),
                sample_card(DiagCode::D1, DiagStatus::Fail),
                sample_card(DiagCode::D2, DiagStatus::Warn),
            ],
            1,
        );
        assert_eq!(report.worst(), Some(DiagStatus::Fail));
        assert!(DiagReport::new(vec![], 1).worst().is_none());
        assert_eq!(DiagStatus::worse(DiagStatus::Ok, DiagStatus::Warn), DiagStatus::Warn);
    }

    #[test]
    fn fix_level_modes() {
        assert_eq!(FixLevel::R1.mode(), "auto");
        assert_eq!(FixLevel::R2.mode(), "suggest");
        assert_eq!(FixLevel::R3.mode(), "authorize");
        assert!(FixLevel::R1.is_auto());
        assert!(!FixLevel::R3.is_auto());
    }
}
