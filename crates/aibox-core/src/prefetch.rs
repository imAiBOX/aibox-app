//! 后台预下载（空闲资源榨干）：已装应用发现新版 → 常驻空闲时把**主安装包**
//! 静默下到内容寻址缓存（`~/.aibox/cache/downloads/<sha256><ext>`），
//! 安装流程 download 阶段查缓存命中即秒过（升级秒装闭环）。
//!
//! - 开关：`~/.aibox/settings.json` 的 `prefetch.enabled`（**默认开**；关=不再触发新预下载，已下缓存保留）
//! - 触发节奏与结果复用 `check_app_updates`（4h 门控在 TS）；本模块只负责选包/落盘/幂等
//! - 全程 best-effort：失败静默（下轮再试）、进度零事件（不打扰用户）、只写任务日志
//! - 单飞：同一时刻至多一个预下载 job（壳侧 job 表以 `prefetch:` 前缀登记）

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::download::{self, DownloadSpec};
use crate::err::{CoreError, CoreResult};
use crate::paths;

/// 预下载取消 panic 标记（进度闭包里轮询取消位 → 中断，保留 `.part` 断点）。
const CANCEL_PANIC: &str = "aibox-prefetch-cancelled";

/// 预下载计划：一个应用的主安装包（routes 当前序首通道起步 + 既有降级链）。
#[derive(Debug, Clone)]
pub struct PrefetchPlan {
    pub app_id: String,
    pub pkg_id: String,
    pub channels: Vec<aibox_net::ChannelUrl>,
    pub sha256: String,
    pub size_bytes: Option<u64>,
    /// 旧式文件名（URL basename）；实际落盘名 = `content_name(dest_name, sha256)`
    pub dest_name: String,
}

/// 预下载结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefetchStatus {
    /// 缓存已命中（幂等跳过，零网络）
    Cached,
    /// 真实下载完成
    Downloaded { bytes: u64, channel: String },
}

// ── 开关（settings.json `prefetch.enabled`，默认开）───────────────────

/// 从已读出的 settings 值取开关（缺省/非布尔 → true = 默认开）。
pub fn enabled_from(settings: &Value) -> bool {
    settings
        .pointer("/prefetch/enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true)
}

/// 读当前开关（真实 settings.json；读不到 → 默认开）。
pub fn enabled() -> bool {
    enabled_from(&crate::region::read_settings())
}

/// 写开关（合并保键 + 原子写 + 回读校验；路径可注入，单测离线）。
pub fn set_enabled_at(path: &Path, enabled: bool) -> CoreResult<()> {
    crate::region::write_settings_at(path, &serde_json::json!({ "prefetch": { "enabled": enabled } }))
        .map(|_| ())
}

/// 写开关（真实 settings.json）。
pub fn set_enabled(enabled: bool) -> CoreResult<()> {
    set_enabled_at(&paths::settings_file(), enabled)
}

// ── 选包（纯函数，可单测）────────────────────────────────────────────

/// URL basename（与 TS `destNameOf` 同构）：去 query/hash 取末段；空 → `download.bin`。
pub fn url_basename(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let seg = path.rsplit('/').next().unwrap_or("");
    if seg.is_empty() {
        "download.bin".to_string()
    } else {
        seg.to_string()
    }
}

/// 包是否匹配本机 platform/arch（arch 缺省/`any` 视为匹配；与 TS `matchesMachine` 同构）。
pub fn matches_machine(pkg: &Value, platform: &str, arch: &str) -> bool {
    let plats: Vec<&str> = pkg
        .get("platforms")
        .and_then(|p| p.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    if !plats.is_empty() && !plats.contains(&platform) {
        return false;
    }
    match pkg.get("arch").and_then(|a| a.as_str()) {
        None | Some("any") | Some("") => true,
        Some(a) => a == arch,
    }
}

fn is_dep_id(id: &str) -> bool {
    id.starts_with("node-") || id.starts_with("git-")
}

/// 从 manifest 选主安装包并展开预下载计划（纯函数）：
/// - 路线：`routes[region]` 首条 → `routePlans[route].steps` 有序（arm64 自动换 `-arm64` 变体）；
///   缺失 → 回落「全部匹配 platform/arch 的 packages」
/// - 主包：steps 顺序里**最后一个非依赖包**（node-/git- 视为依赖，已装不再预下）；
///   全是依赖 → 末位兜底
/// - 要求：有 64 位 sha256 + 至少一条通道（缺 → `None`，调用方静默跳过）
pub fn plan_from_manifest(
    app_id: &str,
    manifest: &Value,
    platform: &str,
    arch: &str,
    region: &str,
) -> Option<PrefetchPlan> {
    let packages = manifest.get("packages")?.as_array()?;
    let by_id: std::collections::HashMap<&str, &Value> = packages
        .iter()
        .filter_map(|p| p.get("id").and_then(|i| i.as_str()).map(|i| (i, p)))
        .collect();

    let mut selected: Vec<&Value> = Vec::new();
    if let Some(route) = manifest
        .pointer(&format!("/routes/{region}"))
        .and_then(|v| v.as_array())
        .and_then(|a| a.first())
        .and_then(|s| s.as_str())
    {
        if let Some(steps) = manifest
            .pointer(&format!("/routePlans/{route}/steps"))
            .and_then(|v| v.as_array())
        {
            selected = steps
                .iter()
                .filter_map(|s| s.as_str())
                .filter_map(|id| {
                    // arm64 机器上 -x64 步骤换 -arm64 变体（与 TS resolveStepId 同构）
                    let resolved = if arch == "arm64" && id.ends_with("-x64") {
                        let alt = format!("{}-arm64", &id[..id.len() - 4]);
                        by_id.get(alt.as_str()).copied().or_else(|| by_id.get(id).copied())
                    } else {
                        by_id.get(id).copied()
                    };
                    resolved
                })
                .filter(|p| matches_machine(p, platform, arch))
                .collect();
        }
    }
    if selected.is_empty() {
        selected = packages
            .iter()
            .filter(|p| matches_machine(p, platform, arch))
            .collect();
    }

    // 主安装包 = 最后一个非依赖包（依赖先行 → 应用本体在尾部）
    let main = selected
        .iter()
        .rev()
        .find(|p| !is_dep_id(p.get("id").and_then(|i| i.as_str()).unwrap_or("")))
        .or_else(|| selected.last())?;

    let sha256 = main.get("sha256").and_then(|s| s.as_str())?.trim().to_lowercase();
    if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channels = download::parse_channels(main.get("channels")?.as_array()?).ok()?;
    let url = channels
        .first()
        .map(|c| c.url.clone())
        .unwrap_or_default();
    Some(PrefetchPlan {
        app_id: app_id.to_string(),
        pkg_id: main
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or("pkg")
            .to_string(),
        channels,
        sha256,
        size_bytes: main.get("sizeBytes").and_then(|s| s.as_u64()),
        dest_name: url_basename(&url),
    })
}

/// 本机常量 → registry 平台/架构标签（与 ScanReport 同形）。
pub fn machine_labels() -> (&'static str, &'static str) {
    let platform = std::env::consts::OS; // windows | macos | linux
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    (platform, arch)
}

/// 取某应用的预下载计划（联网取 manifest：12h 缓存，`check_app_updates` 刚拉过通常直接命中）。
/// 区域 = 当前生效区域（手动固定直接读；auto 走 15min 缓存的三锚点实测）。
pub fn plan_for_app(app_id: &str) -> CoreResult<PrefetchPlan> {
    let (manifest, _) = crate::registry::fetch_app(app_id, false)?;
    let region = crate::region::region_state().effective.as_str().to_string();
    let (platform, arch) = machine_labels();
    plan_from_manifest(app_id, &manifest, platform, arch, &region).ok_or_else(|| {
        CoreError::new(
            "PREFETCH_NO_PACKAGE",
            format!("No downloadable main package for {app_id}"),
        )
    })
}

// ── 执行（缓存命中秒过 / 真下载 / 取消 / 失败清毒）──────────────────

/// 执行预下载（目录可注入，单测离线）：
/// 1. 取消位已置 → `PREFETCH_CANCELLED`（不打扰）；
/// 2. 内容寻址缓存命中（含旧 destName 迁移）→ `Cached`，零网络；
/// 3. 真下载（8MB 分段/断点/降级/S1 sha 零豁免，复用 `download_to_cache`）→
///    进度闭包只轮询取消位、**不发任何事件**；失败 → 清毒后返回错误（调用方静默）。
pub fn run_at(
    dir: &Path,
    plan: &PrefetchPlan,
    cancel: &AtomicBool,
) -> CoreResult<PrefetchStatus> {
    if cancel.load(Ordering::Relaxed) {
        return Err(CoreError::new("PREFETCH_CANCELLED", "prefetch cancelled"));
    }
    if download::resolve_cached(dir, &plan.dest_name, &plan.sha256)?.is_some() {
        return Ok(PrefetchStatus::Cached);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(CoreError::new("PREFETCH_CANCELLED", "prefetch cancelled"));
    }
    let spec = DownloadSpec {
        channels: plan.channels.clone(),
        sha256: plan.sha256.clone(),
        dest_name: plan.dest_name.clone(),
        size_bytes: plan.size_bytes,
        skip_channels: Vec::new(),
        app_id: Some(plan.app_id.clone()),
    };
    let mut progress = |_p: aibox_net::DownloadProgress| {
        if cancel.load(Ordering::Relaxed) {
            panic!("{CANCEL_PANIC}");
        }
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // 真下载固定落真实缓存目录（run_at 的 dir 只服务缓存检查，保持命令语义一致）
        download::download_to_cache(&spec, &mut progress)
    }));
    match outcome {
        Ok(Ok(receipt)) => Ok(PrefetchStatus::Downloaded {
            bytes: receipt.bytes,
            channel: receipt.channel.as_str().to_string(),
        }),
        Ok(Err(e)) => {
            download::purge_invalid(&paths::downloads_dir(), &plan.dest_name, &plan.sha256);
            Err(e)
        }
        Err(payload) => {
            let cancelled = payload
                .downcast_ref::<&'static str>()
                .map(|s| *s == CANCEL_PANIC)
                .or_else(|| payload.downcast_ref::<String>().map(|s| s == CANCEL_PANIC))
                .unwrap_or(false);
            if cancelled {
                Err(CoreError::new("PREFETCH_CANCELLED", "prefetch cancelled"))
            } else {
                download::purge_invalid(&paths::downloads_dir(), &plan.dest_name, &plan.sha256);
                Err(CoreError::new("PREFETCH_PANIC", "prefetch download panicked"))
            }
        }
    }
}

/// 执行预下载（真实缓存目录）。
pub fn run(plan: &PrefetchPlan, cancel: &AtomicBool) -> CoreResult<PrefetchStatus> {
    run_at(&paths::downloads_dir(), plan, cancel)
}

/// 内容寻址落盘路径（任务日志/可观测用）。
pub fn target_path(plan: &PrefetchPlan) -> PathBuf {
    paths::downloads_dir().join(download::content_name(&plan.dest_name, &plan.sha256))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_manifest() -> Value {
        json!({
            "id": "demo",
            "routes": { "global": ["main"], "cn": ["cnroute"] },
            "routePlans": {
                "main": { "steps": ["git-x64", "node-x64", "demo-x64"] },
                "cnroute": { "steps": ["demo-arm64"] }
            },
            "packages": [
                {"id": "git-x64", "name": "Git", "platforms": ["windows","macos","linux"], "sha256": "11".repeat(32), "channels": [{"type":"direct","url":"https://x/git.exe"}]},
                {"id": "node-x64", "name": "Node", "platforms": ["windows"], "sha256": "22".repeat(32), "channels": [{"type":"direct","url":"https://x/node.msi"}]},
                {"id": "demo-x64", "name": "Demo", "platforms": ["windows"], "arch": "x64", "sha256": "33".repeat(32), "sizeBytes": 123, "channels": [{"type":"direct","url":"https://x/Demo-Setup.exe?dl=1"}]},
                {"id": "demo-arm64", "name": "Demo", "platforms": ["macos"], "arch": "arm64", "sha256": "44".repeat(32), "channels": [{"type":"mirror","url":"https://m/Demo.dmg"}]}
            ]
        })
    }

    #[test]
    fn url_basename_matches_dest_name_of() {
        assert_eq!(url_basename("https://a.b/c/d.msi?x=1#f"), "d.msi");
        assert_eq!(url_basename("https://ghfast.top/https://github.com/x/Hub.exe"), "Hub.exe");
        assert_eq!(url_basename("https://a.b/"), "download.bin");
        assert_eq!(url_basename("https://a.b"), "a.b", "与 TS destNameOf 同构");
    }

    #[test]
    fn plan_picks_target_not_deps_and_strips_query() {
        let m = sample_manifest();
        let p = plan_from_manifest("demo", &m, "windows", "x64", "global").unwrap();
        assert_eq!(p.pkg_id, "demo-x64", "主安装包 = 最后一个非依赖包");
        assert_eq!(p.dest_name, "Demo-Setup.exe", "query 去掉");
        assert_eq!(p.sha256, "33".repeat(32));
        assert_eq!(p.size_bytes, Some(123));
        assert_eq!(p.channels.len(), 1);
    }

    #[test]
    fn plan_uses_region_route_and_arm64_variant() {
        let m = sample_manifest();
        // cn 路线首步就是 demo-arm64（macOS/arm64 机器）
        let p = plan_from_manifest("demo", &m, "macos", "arm64", "cn").unwrap();
        assert_eq!(p.pkg_id, "demo-arm64");
        assert_eq!(p.dest_name, "Demo.dmg");
        // global 路线在 arm64 机器上：demo-x64 步骤 → 自动换 demo-arm64（不匹配 macos → 回落全量匹配）
        let p2 = plan_from_manifest("demo", &m, "macos", "arm64", "global").unwrap();
        assert_eq!(p2.pkg_id, "demo-arm64");
    }

    #[test]
    fn plan_falls_back_to_all_matching_packages() {
        let m = json!({
            "packages": [
                {"id": "only-x64", "platforms": ["windows"], "sha256": "aa".repeat(32),
                 "channels": [{"type":"direct","url":"https://x/o.exe"}]}
            ]
        });
        let p = plan_from_manifest("demo", &m, "windows", "x64", "global").unwrap();
        assert_eq!(p.pkg_id, "only-x64");
        // 平台不匹配 → 无包
        assert!(plan_from_manifest("demo", &m, "linux", "x64", "global").is_none());
    }

    #[test]
    fn plan_requires_valid_sha_and_channels() {
        let mut m = sample_manifest();
        m["packages"][2]["sha256"] = json!("tooshort");
        assert!(
            plan_from_manifest("demo", &m, "windows", "x64", "global").is_none(),
            "坏 sha → 不预下"
        );
        let mut m2 = sample_manifest();
        m2["packages"][2]["channels"] = json!([]);
        // channels 空 → parse_channels 报错 → None
        assert!(plan_from_manifest("demo", &m2, "windows", "x64", "global").is_none());
        // manifest 无 packages → None
        assert!(plan_from_manifest("demo", &json!({}), "windows", "x64", "global").is_none());
    }

    #[test]
    fn enabled_defaults_on_and_roundtrips() {
        assert!(enabled_from(&json!({})), "缺省 = 默认开");
        assert!(enabled_from(&json!({"prefetch": {"enabled": true}})));
        assert!(!enabled_from(&json!({"prefetch": {"enabled": false}})));
        assert!(enabled_from(&json!({"prefetch": {"enabled": "x"}})), "非布尔按默认开");

        let d = std::env::temp_dir().join(format!("aibox-pf-set-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("settings.json");
        std::fs::write(&f, r#"{"net":{"region":"cn"}}"#).unwrap();
        set_enabled_at(&f, false).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["prefetch"]["enabled"], false);
        assert_eq!(v["net"]["region"], "cn", "保键（区域设置不丢）");
        assert!(!enabled_from(&v));
        set_enabled_at(&f, true).unwrap();
        let v2: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert!(enabled_from(&v2));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn run_at_cache_hit_is_idempotent_offline() {
        let d = std::env::temp_dir().join(format!("aibox-pf-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let sha = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"; // sha256("hello")
        let plan = PrefetchPlan {
            app_id: "demo".into(),
            pkg_id: "demo-x64".into(),
            channels: vec![aibox_net::ChannelUrl::new(
                aibox_net::ChannelType::Direct,
                "https://x.invalid/Demo-Setup.exe",
            )],
            sha256: sha.to_string(),
            size_bytes: None,
            dest_name: "Demo-Setup.exe".into(),
        };
        std::fs::write(d.join(download::content_name("Demo-Setup.exe", sha)), b"hello").unwrap();
        let cancel = AtomicBool::new(false);
        // 命中 → Cached（零网络；通道指向 invalid 域也无所谓）
        assert_eq!(run_at(&d, &plan, &cancel).unwrap(), PrefetchStatus::Cached);
        // 幂等：再来一次仍 Cached，文件不重复
        assert_eq!(run_at(&d, &plan, &cancel).unwrap(), PrefetchStatus::Cached);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        // 取消位 → 不碰网络直接取消
        cancel.store(true, Ordering::Relaxed);
        let e = run_at(&d, &plan, &cancel).unwrap_err();
        assert_eq!(e.code, "PREFETCH_CANCELLED");
        let _ = std::fs::remove_dir_all(&d);
    }
}
