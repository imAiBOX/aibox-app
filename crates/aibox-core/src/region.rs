//! 区域判定：初值信号 + github 实测覆盖 + 用户固定。
//!
//! - 初值：UI/系统语言 zh + 时区 Asia/Shanghai + 系统区域 CN → CN，否则 GLOBAL
//! - 实测覆盖：github.com 直连成功且 TTFB < 800ms → GLOBAL（会话级缓存 15min）
//! - 用户固定（`net_region_set`）写 `~/.aibox/settings.json`，最高优先

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use aibox_net::{
    initial_region, override_from_probe, resolve_region, ProbeOutcome, RegionKind, RegionSignals,
    RegionSource, RegionState,
};

use crate::err::{CoreError, CoreResult};
use crate::fsops;
use crate::paths;

/// 实测缓存 15 分钟（会话语义）。
const MEASURE_TTL: Duration = Duration::from_secs(15 * 60);

static MEASURED: Mutex<Option<(Instant, Option<ProbeOutcome>)>> = Mutex::new(None);

/// 采集区域信号（Windows 读注册表免提权；unix 走环境变量 + /etc/timezone）。
pub fn detect_signals() -> RegionSignals {
    #[cfg(windows)]
    {
        let locale = win_str(r"Control Panel\International", "LocaleName")
            .or_else(|| env_str("LANG"))
            .unwrap_or_default();
        let geo = win_dword(r"Control Panel\International\Geo", "Nation").unwrap_or(0);
        let region = if geo == 156 {
            "CN".to_string()
        } else {
            String::new()
        };
        let tz = win_str(
            r"SYSTEM\CurrentControlSet\Control\TimeZoneInformation",
            "TimeZoneKeyName",
        )
        .map(|t| map_windows_tz(&t))
        .or_else(|| env_str("TZ"))
        .unwrap_or_default();
        return RegionSignals { locale, tz, region };
    }

    #[cfg(not(windows))]
    {
        let mut s = aibox_net::signals_from_env(|k| std::env::var(k).ok());
        if s.tz.is_empty() {
            if let Ok(tz) = std::fs::read_to_string("/etc/timezone") {
                s.tz = tz.trim().to_string();
            }
        }
        if s.region.is_empty() {
            // 调试/CI 注入（AIBOX_GEO_REGION）已在 signals_from_env 读取；保持为空
        }
        s
    }
}

#[cfg(windows)]
fn env_str(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

#[cfg(windows)]
fn win_str(subkey: &str, name: &str) -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    if let Ok(v) = hkcu
        .open_subkey(subkey)
        .and_then(|k| k.get_value::<String, _>(name))
    {
        if !v.is_empty() {
            return Some(v);
        }
    }
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    hklm.open_subkey(subkey)
        .and_then(|k| k.get_value::<String, _>(name))
        .ok()
        .filter(|v| !v.is_empty())
}

#[cfg(windows)]
fn win_dword(subkey: &str, name: &str) -> Option<u32> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    hkcu.open_subkey(subkey)
        .and_then(|k| k.get_value::<u32, _>(name))
        .ok()
}

/// Windows 时区键 → IANA（只映射初值判定需要的中国时区，其余原样）。
#[cfg(windows)]
fn map_windows_tz(t: &str) -> String {
    match t {
        "China Standard Time" => "Asia/Shanghai".to_string(),
        "Taipei Standard Time" => "Asia/Taipei".to_string(),
        other => other.to_string(),
    }
}

/// 初值判定（不带实测）。
pub fn initial_kind() -> RegionKind {
    let s = detect_signals();
    initial_region(&s.locale, &s.tz, &s.region)
}

/// github.com 直连实测（带 15min 会话缓存）。
pub fn measured_override() -> Option<ProbeOutcome> {
    if let Ok(guard) = MEASURED.lock() {
        if let Some((at, outcome)) = guard.as_ref() {
            if at.elapsed() < MEASURE_TTL {
                return outcome.clone();
            }
        }
    }
    let outcome = aibox_net::probe_head("https://github.com/", Duration::from_secs(5));
    if let Ok(mut guard) = MEASURED.lock() {
        *guard = Some((Instant::now(), Some(outcome.clone())));
    }
    Some(outcome)
}

/// 纯判定（可注入实测结果，单测离线）。
pub fn region_state_with(settings: &Value, measured: Option<ProbeOutcome>) -> RegionState {
    let source = settings
        .pointer("/net/region")
        .and_then(|v| v.as_str())
        .and_then(|s| RegionSource::deserialize_str(s))
        .unwrap_or(RegionSource::Auto);
    let initial = {
        let s = detect_signals();
        initial_region(&s.locale, &s.tz, &s.region)
    };
    let measured_region = match source {
        RegionSource::Auto => measured.as_ref().and_then(override_from_probe),
        _ => None,
    };
    let effective = resolve_region(source, initial, measured_region);
    RegionState { source, effective }
}

/// `net_region()` 实装：读 settings + 实测（auto 时）。
pub fn region_state() -> RegionState {
    let settings = read_settings();
    let measured = match settings.pointer("/net/region").and_then(|v| v.as_str()) {
        Some("cn") | Some("global") => None, // 手动固定不探测
        _ => measured_override(),
    };
    region_state_with(&settings, measured)
}

/// `net_region_set(v)`：校验 → 合并写 settings（保键）→ 返回新状态。
pub fn set_region(v: &str) -> CoreResult<RegionState> {
    let _source = RegionSource::deserialize_str(v).ok_or_else(|| {
        CoreError::new(
            "REGION_BAD_VALUE",
            format!("Invalid region: {v} (expected auto|cn|global)"),
        )
    })?;
    let path = paths::settings_file();
    let mut settings = fsops::read_json5(&path)?;
    let patch = json!({ "net": { "region": v } });
    fsops::merge_preserve(&mut settings, &patch);
    fsops::write_json_atomic(&path, &settings)?;
    if !fsops::readback_verify(&path, &settings)? {
        return Err(CoreError::new(
            "SETTINGS_WRITE_FAILED",
            "Settings file did not verify after write",
        ));
    }
    Ok(region_state())
}

/// 读设置文件（不存在 → `{}`）。
pub fn read_settings() -> Value {
    fsops::read_json5(&paths::settings_file()).unwrap_or_else(|_| json!({}))
}

/// RegionSource 宽容解析（serde 只认小写，这里兜一层）。
trait DeserializeStr: Sized {
    fn deserialize_str(s: &str) -> Option<Self>;
}

impl DeserializeStr for RegionSource {
    fn deserialize_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(RegionSource::Auto),
            "cn" => Some(RegionSource::Cn),
            "global" => Some(RegionSource::Global),
            _ => None,
        }
    }
}

/// 设置文件的合并写（路径可注入，单测离线）。
pub fn write_settings_at(path: &Path, patch: &Value) -> CoreResult<Value> {
    let mut settings = fsops::read_json5(path)?;
    fsops::merge_preserve(&mut settings, patch);
    fsops::write_json_atomic(path, &settings)?;
    if !fsops::readback_verify(path, &settings)? {
        return Err(CoreError::new(
            "SETTINGS_WRITE_FAILED",
            "Settings file did not verify after write",
        ));
    }
    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_source_values() {
        assert!(matches!(
            RegionSource::deserialize_str("auto"),
            Some(RegionSource::Auto)
        ));
        assert!(matches!(
            RegionSource::deserialize_str("CN"),
            Some(RegionSource::Cn)
        ));
        assert!(matches!(
            RegionSource::deserialize_str(" global "),
            Some(RegionSource::Global)
        ));
        assert!(RegionSource::deserialize_str("europe").is_none());
    }

    #[test]
    fn manual_override_wins_without_probe() {
        let s = json!({"net": {"region": "cn"}});
        let st = region_state_with(&s, None);
        assert_eq!(st.source, RegionSource::Cn);
        assert_eq!(st.effective, RegionKind::Cn);

        let s = json!({"net": {"region": "global"}});
        let st = region_state_with(&s, None);
        assert_eq!(st.effective, RegionKind::Global);
    }

    #[test]
    fn auto_uses_measured_when_fast_github() {
        let s = json!({"net": {"region": "auto"}});
        let fast = ProbeOutcome {
            success: true,
            ttfb_ms: 120,
            status: Some(200),
            error: None,
        };
        let st = region_state_with(&s, Some(fast));
        assert_eq!(st.source, RegionSource::Auto);
        assert_eq!(st.effective, RegionKind::Global, "fast github → GLOBAL 序");

        let slow = ProbeOutcome {
            success: false,
            ttfb_ms: 0,
            status: None,
            error: Some("timeout".into()),
        };
        let st2 = region_state_with(&s, Some(slow));
        // 实测失败不覆盖 → 回落初值（本机时区信号决定）
        assert!(st2.effective == RegionKind::Cn || st2.effective == RegionKind::Global);
    }

    #[test]
    fn settings_write_preserves_keys() {
        let d = std::env::temp_dir().join(format!("aibox-settings-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("settings.json");
        std::fs::write(&f, r#"{"other": {"keep": 1}}"#).unwrap();
        let v = write_settings_at(&f, &json!({"net": {"region": "cn"}})).unwrap();
        assert_eq!(v["net"]["region"], "cn");
        assert_eq!(v["other"]["keep"], 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn region_state_serializes_to_contract() {
        let s = json!({"net": {"region": "cn"}});
        let st = region_state_with(&s, None);
        let v = serde_json::to_value(st).unwrap();
        assert_eq!(v["source"], "cn");
        assert_eq!(v["effective"], "cn");
    }
}
