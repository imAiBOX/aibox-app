//! 区域判定：初值信号 + 三锚点实测覆盖 + 用户固定。
//!
//! - 初值：UI/系统语言 zh + 时区 Asia/Shanghai + 系统区域 CN → CN，否则 GLOBAL
//! - 实测覆盖：三锚点并行实测（github 主站 / release 资产全链 / npmmirror），
//!   主判据 = release 资产全链（见 `aibox_net::classify_multi`），会话级缓存 15min；
//!   实测结果与 TTL 缓存保留在 `MEASURED`（可观测 → `RegionState.anchors`）
//! - 用户固定（`net_region_set`）写 `~/.aibox/settings.json`，最高优先

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use aibox_net::{
    initial_region, probe_head, resolve_region, ProbeOutcome, RegionKind, RegionProbes,
    RegionSignals, RegionSource, RegionState,
};

use crate::err::{CoreError, CoreResult};
use crate::fsops;
use crate::paths;

/// 实测缓存 15 分钟（会话语义）。
const MEASURE_TTL: Duration = Duration::from_secs(15 * 60);
/// 单锚点探测超时（与 probe_head 基线一致）。
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// 锚点 A：github 主站（翻墙/优化线路信号，不单独定案）。
const ANCHOR_GITHUB: &str = "https://github.com/";
/// 锚点 C：npmmirror 国内镜像（辅助记录，不单独触发覆盖）。
const ANCHOR_NPMIRROR: &str = "https://registry.npmmirror.com/";
/// 锚点 B 兑底：GitHub release 资产（主判据）。`releases/latest/download/<资产名>`
/// 形式随最新 release 自动跟随；registry 缓存可用时优先用缓存里的真实资产 URL。
const RELEASE_FALLBACKS: &[&str] = &[
    "https://github.com/openclaw/openclaw-windows-node/releases/latest/download/OpenClawCompanion-Setup-x64.exe",
    "https://github.com/openclaw/openclaw-windows-node/releases/download/v2026.11.1/OpenClawCompanion-Setup-x64.exe",
];

/// 实测三锚点 + TTL（结构扩展：可观测与判定同源）。
static MEASURED: Mutex<Option<(Instant, RegionProbes)>> = Mutex::new(None);

fn probe_failed(reason: &str) -> ProbeOutcome {
    ProbeOutcome {
        success: false,
        ttfb_ms: 0,
        status: None,
        error: Some(reason.to_string()),
    }
}

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

/// 锚点 B 候选：优先 registry 缓存里 openclaw hub-x64 的 direct 资产 URL（真实大文件
/// 端点），兑底固定列表。全部要求 `https://github.com/` 前缀（保证跟到
/// objects.githubusercontent.com 资产域）。
pub fn release_asset_candidates() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(doc) = crate::registry::read_cached_app("openclaw") {
        if let Some(pkgs) = doc.get("packages").and_then(|p| p.as_array()) {
            for pkg in pkgs {
                let id = pkg.get("id").and_then(|i| i.as_str()).unwrap_or("");
                if !id.contains("hub-win-x64") {
                    continue;
                }
                for ch in pkg.get("channels").and_then(|c| c.as_array()).into_iter().flatten() {
                    if ch.get("type").and_then(|t| t.as_str()) != Some("direct") {
                        continue;
                    }
                    if let Some(u) = ch.get("url").and_then(|u| u.as_str()) {
                        if u.starts_with("https://github.com/") && !out.iter().any(|x| x == u) {
                            out.push(u.to_string());
                        }
                    }
                }
            }
        }
    }
    for f in RELEASE_FALLBACKS {
        if !out.iter().any(|x| x == f) {
            out.push(f.to_string());
        }
    }
    out
}

/// 三锚点并行实测（每锚点 5s 超时；锚点 B 多候选取最优：任一成功取最快，全败取首败）。
pub fn measure_three() -> RegionProbes {
    let b_handles: Vec<_> = release_asset_candidates()
        .into_iter()
        .map(|u| {
            std::thread::spawn(move || probe_head(&u, PROBE_TIMEOUT))
        })
        .collect();
    let a = std::thread::spawn(|| probe_head(ANCHOR_GITHUB, PROBE_TIMEOUT));
    let c = std::thread::spawn(|| probe_head(ANCHOR_NPMIRROR, PROBE_TIMEOUT));

    let mut release: Option<ProbeOutcome> = None;
    for h in b_handles {
        if let Ok(o) = h.join() {
            let better = match &release {
                None => true,
                Some(cur) => match (cur.success, o.success) {
                    (false, true) => true, // 从失败候选换到成功候选
                    (true, true) => o.ttfb_ms < cur.ttfb_ms,
                    _ => false,
                },
            };
            if better {
                release = Some(o);
            }
        }
    }

    RegionProbes {
        github: a
            .join()
            .unwrap_or_else(|_| probe_failed("github probe panicked")),
        release_asset: release
            .unwrap_or_else(|| probe_failed("no release asset candidate available")),
        npmmirror: c
            .join()
            .unwrap_or_else(|_| probe_failed("npmmirror probe panicked")),
    }
}

/// 三锚点实测（带 15min 会话缓存；TTL 内直接命中 `MEASURED`）。
pub fn measured_probes() -> RegionProbes {
    if let Ok(guard) = MEASURED.lock() {
        if let Some((at, probes)) = guard.as_ref() {
            if at.elapsed() < MEASURE_TTL {
                return probes.clone();
            }
        }
    }
    let probes = measure_three();
    if let Ok(mut guard) = MEASURED.lock() {
        *guard = Some((Instant::now(), probes.clone()));
    }
    probes
}

/// 实测区域覆盖（15min 缓存）：三锚点判定，`None` = 不覆盖（回落初值）。
pub fn measured_override() -> Option<RegionKind> {
    measured_probes().classify()
}

/// 纯判定（可注入实测结果，单测离线）。
pub fn region_state_with(settings: &Value, measured: Option<RegionKind>) -> RegionState {
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
        RegionSource::Auto => measured,
        _ => None,
    };
    let effective = resolve_region(source, initial, measured_region);
    RegionState {
        source,
        effective,
        anchors: None,
    }
}

/// `net_region()` 实装：读 settings + 三锚点实测（auto 时）。
/// 实测三锚点结果写进 `anchors`（可观测；手动固定不探测 → 省略）。
pub fn region_state() -> RegionState {
    let settings = read_settings();
    let manual = matches!(
        settings.pointer("/net/region").and_then(|v| v.as_str()),
        Some("cn") | Some("global")
    );
    if manual {
        return region_state_with(&settings, None);
    }
    let probes = measured_probes();
    let mut state = region_state_with(&settings, probes.classify());
    state.anchors = Some(probes.to_anchors());
    state
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
    fn auto_uses_measured_when_asset_endpoint_fast() {
        let s = json!({"net": {"region": "auto"}});
        // 三锚点判定：资产链通且快 → Some(Global)
        let st = region_state_with(&s, Some(RegionKind::Global));
        assert_eq!(st.source, RegionSource::Auto);
        assert_eq!(st.effective, RegionKind::Global, "资产链可达 → GLOBAL 序");
        assert!(st.anchors.is_none(), "注入判定不带实测锚点");

        // 实测不覆盖（如国内典型：主站通/资产挂）→ 回落初值（本机时区信号决定）
        let st2 = region_state_with(&s, None);
        assert!(st2.effective == RegionKind::Cn || st2.effective == RegionKind::Global);
    }

    #[test]
    fn manual_source_ignores_measured_and_omits_anchors() {
        let s = json!({"net": {"region": "cn"}});
        let st = region_state_with(&s, Some(RegionKind::Global));
        assert_eq!(st.effective, RegionKind::Cn, "手动设置最高优先");
        assert!(st.anchors.is_none());
    }

    #[test]
    fn release_asset_candidates_are_github_urls() {
        let c = release_asset_candidates();
        assert!(!c.is_empty(), "至少有兑底候选");
        for u in &c {
            assert!(
                u.starts_with("https://github.com/"),
                "锚点 B 必须是 GitHub release 资产（会跟到 objects.githubusercontent.com）: {u}"
            );
        }
    }

    /// 真实联网 smoke（不入 CI）：三锚点实测 + 判定打印。
    /// 用法：`cargo test -p aibox-core measure_three_live -- --ignored --nocapture`
    #[test]
    #[ignore = "live network smoke"]
    fn measure_three_live() {
        let probes = measure_three();
        let anchors = probes.to_anchors();
        println!("anchors = {}", serde_json::to_string_pretty(&anchors).unwrap());
        println!("classify = {:?}", probes.classify());
        assert!(
            anchors.github.ok || anchors.release_asset.ok || anchors.npmmirror.ok,
            "至少一个锚点可达"
        );
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
