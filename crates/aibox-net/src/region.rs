//! 区域判定与通道排序（RegionPolicy）。
//!
//! - **初值**：UI 语言 zh + 系统时区 Asia/Shanghai + 系统区域 CN → CN 序，否则 GLOBAL 序。
//!   三个信号全部以参数注入（纯函数），系统信号读取见 [`detect_initial_region`]。
//! - **实测覆盖**：github.com 直连成功且 TTFB < 800ms → 采用 GLOBAL 序
//!   （避免给翻墙中的用户强推国内镜像反而绕路）。
//! - **排序**：CN 序 = MIRROR > PROXY > DIRECT；GLOBAL 序 = DIRECT > 其余；
//!   CUSTOM 恒为末位（用户自定义通道由用户显式启用，不参与区域偏好）。
//! - 用户可在设置中手动固定（auto/cn/global → [`RegionSource`]）。

use serde::{Deserialize, Serialize};

use crate::channel::ChannelType;
use crate::probe::ProbeOutcome;

/// 有效区域序（`net_region().effective`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegionKind {
    /// 中国大陆序：MIRROR > PROXY > DIRECT
    Cn,
    /// 国际序：DIRECT > 其余
    Global,
}

impl RegionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RegionKind::Cn => "cn",
            RegionKind::Global => "global",
        }
    }
}

/// 区域来源（`net_region().source`）：用户设置 or 自动判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RegionSource {
    /// 自动（初值 + 实测覆盖）
    Auto,
    /// 用户手动固定为大陆
    Cn,
    /// 用户手动固定为国际
    Global,
}

impl RegionSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            RegionSource::Auto => "auto",
            RegionSource::Cn => "cn",
            RegionSource::Global => "global",
        }
    }
}

/// `net_region()` 返回结构。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionState {
    pub source: RegionSource,
    pub effective: RegionKind,
}

/// 区域判定的三个输入信号（可注入，纯函数判定）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RegionSignals {
    /// UI/系统语言，如 `zh_CN.UTF-8`、`en-US`
    pub locale: String,
    /// 系统时区，如 `Asia/Shanghai`
    pub tz: String,
    /// 系统区域/地理ID，如 `CN`（拿不到则为空串）
    pub region: String,
}

/// 初值判定：zh + Asia/Shanghai + CN 三条件同时满足 → CN，否则 GLOBAL。
pub fn initial_region(locale: &str, tz: &str, region: &str) -> RegionKind {
    let zh = locale
        .trim()
        .to_ascii_lowercase()
        .trim_start_matches(['"', '\''])
        .starts_with("zh");
    let shanghai = tz.trim() == "Asia/Shanghai";
    let cn = region.trim().eq_ignore_ascii_case("CN");
    if zh && shanghai && cn {
        RegionKind::Cn
    } else {
        RegionKind::Global
    }
}

/// 实测覆盖：github.com 直连成功且 TTFB < 800ms → 采用 GLOBAL 序。
/// 返回 `None` 表示不覆盖（保留初值）。
pub fn override_from_probe(github_direct: &ProbeOutcome) -> Option<RegionKind> {
    if github_direct.success && github_direct.ttfb_ms < 800 {
        Some(RegionKind::Global)
    } else {
        None
    }
}

/// 合成最终生效区域：用户手动固定优先，AUTO = 实测覆盖（若有）否则初值。
pub fn resolve_region(
    source: RegionSource,
    initial: RegionKind,
    measured: Option<RegionKind>,
) -> RegionKind {
    match source {
        RegionSource::Cn => RegionKind::Cn,
        RegionSource::Global => RegionKind::Global,
        RegionSource::Auto => measured.unwrap_or(initial),
    }
}

/// 通道排序（纯函数）：`kind` = 该资源可用通道集（矩阵行），`region` = 生效区域。
///
/// - CN 序：MIRROR > PROXY > DIRECT > CUSTOM
/// - GLOBAL 序：DIRECT > 其余（保持调用方相对次序）> CUSTOM（恒末位）
///
/// 结果去重（保留首次出现），输入中的通道原样保留。
pub fn order_channels(kind: &[ChannelType], region: RegionKind) -> Vec<ChannelType> {
    let mut seen = [false; 4];
    let mut deduped: Vec<ChannelType> = Vec::with_capacity(kind.len());
    for &c in kind {
        let idx = match c {
            ChannelType::Direct => 0,
            ChannelType::Mirror => 1,
            ChannelType::Proxy => 2,
            ChannelType::Custom => 3,
        };
        if !seen[idx] {
            seen[idx] = true;
            deduped.push(c);
        }
    }
    match region {
        RegionKind::Cn => {
            // 显式偏序 + 稳定排序
            deduped.sort_by_key(|c| match c {
                ChannelType::Mirror => 0u8,
                ChannelType::Proxy => 1,
                ChannelType::Direct => 2,
                ChannelType::Custom => 3,
            });
        }
        RegionKind::Global => {
            // DIRECT 首位；其余保持输入相对次序；CUSTOM 恒末（稳定排序）
            deduped.sort_by_key(|c| match c {
                ChannelType::Direct => (0u8, 0u8),
                ChannelType::Custom => (2, 0),
                _ => (1, 0),
            });
        }
    }
    deduped
}

/// 从环境变量读取区域信号（可注入 getter，便于单测）。
///
/// - locale：`LC_ALL` → `LC_MESSAGES` → `LANG`，剥离 `.UTF-8`/`@modifier`
/// - tz：`TZ`（剥离前导 `:`）
/// - region：`AIBOX_GEO_REGION`（调试/CI 注入用）
pub fn signals_from_env(get: impl Fn(&str) -> Option<String>) -> RegionSignals {
    let raw_locale = get("LC_ALL")
        .filter(|v| !v.trim().is_empty())
        .or_else(|| get("LC_MESSAGES").filter(|v| !v.trim().is_empty()))
        .or_else(|| get("LANG").filter(|v| !v.trim().is_empty()))
        .unwrap_or_default();
    let locale = raw_locale
        .split('@')
        .next()
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    let tz = get("TZ").unwrap_or_default().trim_start_matches(':').to_string();
    let region = get("AIBOX_GEO_REGION").unwrap_or_default();
    RegionSignals {
        locale,
        tz,
        region,
    }
}

/// 读取本机环境信号并给出初值（best-effort，纯判定见 [`initial_region`]）。
///
/// TODO(Windows)：region 用 `GetUserGeoID`（免提权）、tz 用 `GetTimeZoneInformation`
/// 补齐（cfg(windows) 分支，Linux CI 不编译）；macOS 用 `systemsetup -gettimezone`
/// （读取不需 root）。当前拿不到的信号为空串 → 走 GLOBAL 安全默认。
pub fn detect_initial_region() -> RegionKind {
    let signals = signals_from_env(|k| std::env::var(k).ok());
    initial_region(&signals.locale, &signals.tz, &signals.region)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::ProbeOutcome;

    fn ok(ttfb: u64) -> ProbeOutcome {
        ProbeOutcome {
            success: true,
            ttfb_ms: ttfb,
            status: Some(200),
            error: None,
        }
    }

    fn fail() -> ProbeOutcome {
        ProbeOutcome {
            success: false,
            ttfb_ms: 5000,
            status: None,
            error: Some("timeout".into()),
        }
    }

    #[test]
    fn initial_region_requires_all_three_signals() {
        assert_eq!(initial_region("zh_CN.UTF-8", "Asia/Shanghai", "CN"), RegionKind::Cn);
        assert_eq!(initial_region("zh_CN", "Asia/Shanghai", "CN"), RegionKind::Cn);
        // 任一不满足 → global
        assert_eq!(initial_region("en_US", "Asia/Shanghai", "CN"), RegionKind::Global);
        assert_eq!(initial_region("zh_CN", "Europe/Paris", "CN"), RegionKind::Global);
        assert_eq!(initial_region("zh_CN", "Asia/Shanghai", "US"), RegionKind::Global);
        assert_eq!(initial_region("", "", ""), RegionKind::Global);
    }

    #[test]
    fn measured_override_github_fast_direct_wins_global() {
        assert_eq!(override_from_probe(&ok(799)), Some(RegionKind::Global));
        assert_eq!(override_from_probe(&ok(0)), Some(RegionKind::Global));
        // 边界：800ms 不覆盖
        assert_eq!(override_from_probe(&ok(800)), None);
        assert_eq!(override_from_probe(&ok(5000)), None);
        assert_eq!(override_from_probe(&fail()), None);
    }

    #[test]
    fn resolve_region_manual_override_beats_auto() {
        let initial = RegionKind::Cn;
        let measured = Some(RegionKind::Global);
        assert_eq!(resolve_region(RegionSource::Auto, initial, measured), RegionKind::Global);
        assert_eq!(resolve_region(RegionSource::Auto, initial, None), RegionKind::Cn);
        assert_eq!(resolve_region(RegionSource::Cn, initial, measured), RegionKind::Cn);
        assert_eq!(resolve_region(RegionSource::Global, RegionKind::Cn, None), RegionKind::Global);
    }

    #[test]
    fn order_channels_cn_is_mirror_proxy_direct() {
        let full = ChannelType::all();
        assert_eq!(
            order_channels(&full, RegionKind::Cn),
            vec![
                ChannelType::Mirror,
                ChannelType::Proxy,
                ChannelType::Direct,
                ChannelType::Custom
            ]
        );
    }

    #[test]
    fn order_channels_global_is_direct_then_rest_custom_last() {
        let full = ChannelType::all();
        assert_eq!(
            order_channels(&full, RegionKind::Global),
            vec![
                ChannelType::Direct,
                ChannelType::Mirror,
                ChannelType::Proxy,
                ChannelType::Custom
            ]
        );
        // 其余保持输入相对次序
        let subset = [ChannelType::Proxy, ChannelType::Mirror, ChannelType::Direct];
        assert_eq!(
            order_channels(&subset, RegionKind::Global),
            vec![
                ChannelType::Direct,
                ChannelType::Proxy,
                ChannelType::Mirror
            ]
        );
    }

    #[test]
    fn order_channels_dedup_and_subset() {
        let subset = [ChannelType::Direct, ChannelType::Mirror, ChannelType::Direct];
        assert_eq!(
            order_channels(&subset, RegionKind::Cn),
            vec![ChannelType::Mirror, ChannelType::Direct]
        );
        assert!(order_channels(&[], RegionKind::Cn).is_empty());
    }

    #[test]
    fn region_serde_matches_command_contract() {
        // net_region(): {source:'auto'|'cn'|'global', effective:'cn'|'global'}
        let state = RegionState {
            source: RegionSource::Auto,
            effective: RegionKind::Cn,
        };
        assert_eq!(
            serde_json::to_string(&state).unwrap(),
            r#"{"source":"auto","effective":"cn"}"#
        );
        let state2 = RegionState {
            source: RegionSource::Global,
            effective: RegionKind::Global,
        };
        assert_eq!(
            serde_json::to_string(&state2).unwrap(),
            r#"{"source":"global","effective":"global"}"#
        );
        assert_eq!(
            serde_json::from_str::<RegionState>(r#"{"source":"cn","effective":"cn"}"#).unwrap(),
            RegionState {
                source: RegionSource::Cn,
                effective: RegionKind::Cn,
            }
        );
    }

    #[test]
    fn signals_from_env_strips_encoding() {
        let signals = signals_from_env(|k| match k {
            "LC_ALL" => Some(String::new()),
            "LC_MESSAGES" => Some(String::new()),
            "LANG" => Some("zh_CN.UTF-8@pinyin".into()),
            "TZ" => Some(":Asia/Shanghai".into()),
            "AIBOX_GEO_REGION" => Some("CN".into()),
            _ => None,
        });
        assert_eq!(signals.locale, "zh_CN");
        assert_eq!(signals.tz, "Asia/Shanghai");
        assert_eq!(signals.region, "CN");
        assert_eq!(
            initial_region(&signals.locale, &signals.tz, &signals.region),
            RegionKind::Cn
        );
    }
}
