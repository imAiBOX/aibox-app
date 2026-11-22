//! CONFIG C5 写入流程（`config_write`）：
//! 备份 `.bak.<ts>` → JSON5 合并保留未知键 → 原子写 → 回读校验 → 脱敏返回。
//!
//! 写入布局（与 OpenClaw `models.providers` 配置路径对齐；字段名在 A 档联调时复核）：
//! ```jsonc
//! { "models": { "providers": { "<provider>": { "apiKey": "...", "baseUrl": "...", "model": "..." } } } }
//! ```

use std::path::PathBuf;

use serde::Serialize;
use serde_json::{json, Value};

use crate::err::{CoreError, CoreResult};
use crate::fsops;
use crate::paths;
use crate::config_tpl::{Ctx, Template};
use crate::registry;

/// `config_write` 输入（ConfigWriteInput）。
#[derive(Debug, Clone, Default)]
pub struct ConfigWriteInput {
    pub app_id: String,
    pub provider: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub target_path: Option<String>,
}

/// `config_write` 返回（`{backupPath, ok, mergedKeys?}`）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigWriteResult {
    /// 备份路径（首次写入无原文件 → 空串）
    pub backup_path: String,
    /// 回读校验通过
    pub ok: bool,
    /// 本次写入/变更的键路径（**不含任何值**）
    pub merged_keys: Vec<String>,
}

/// 六 provider 合法值。
pub const PROVIDERS: [&str; 6] = [
    "openrouter",
    "anthropic",
    "openai",
    "google",
    "claude-oauth",
    "ollama",
];

/// 解析目标配置文件：
/// `targetPath` 显式值 > registry 缓存 configTarget > `~/.config/<appId>/<appId>.json5`。
pub fn resolve_target(input: &ConfigWriteInput) -> CoreResult<PathBuf> {
    if let Some(t) = &input.target_path {
        if !t.trim().is_empty() {
            return Ok(paths::expand(t));
        }
    }
    // registry 缓存优先（不发起网络：C5 是本地写操作）
    if let Ok(v) = registry::read_cached_app(&input.app_id) {
        if let Some(t) = v
            .pointer(&format!("/modelProviders/{}/configTarget", input.provider))
            .and_then(|x| x.as_str())
        {
            return Ok(paths::expand(t));
        }
    }
    if input.app_id.trim().is_empty() {
        return Err(CoreError::new(
            "CONFIG_NO_APP",
            "appId is required when no targetPath is given",
        ));
    }
    Ok(paths::expand(&format!(
        "~/.config/{0}/{0}.json5",
        input.app_id.trim()
    )))
}

/// 解析目标配置文件（**模板优先**）：
/// `configWrite.file` > 调用方 `targetPath` > registry 的 `configTarget` > 兜底。
fn resolve_target_with_template(
    input: &ConfigWriteInput,
    template: Option<&Template>,
) -> CoreResult<PathBuf> {
    if let Some(t) = template {
        if let Some(f) = &t.file {
            if !f.trim().is_empty() {
                return Ok(paths::expand(f));
            }
        }
    }
    resolve_target(input)
}

/// 该 provider 在此应用下**是否可由 AiBOX 自动写入**。
///
/// **为什么必须有这个闸门**：通用写入布局是 JSON5（`models.providers.*`），
/// 而各应用的真实配置格式**五花八门**（json / yaml / yml / toml / sqlite / 无扩展名）。
/// 对不认识的格式硬写，会把用户的 YAML/TOML/SQLite 配置**覆盖成 JSON** → 应用直接坏掉。
/// registry 的 `modelProviders.<p>.configWritable` 是真源：只有验证过写结构的应用才为 true。
pub fn provider_writable(input: &ConfigWriteInput) -> CoreResult<bool> {
    match registry::read_cached_app(&input.app_id) {
        Ok(v) => Ok(provider_writable_in(&v, &input.provider)),
        // registry 缓存里查不到 → 保守：只有 json/json5 目标才允许（见 write_allowed）
        Err(_) => Ok(false),
    }
}

/// 目标是不是「通用 JSON5 结构」能安全写的文件（只看扩展名）。
fn is_json_target(p: &PathBuf) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()),
        Some("json") | Some("json5")
    )
}

/// **写入闸门**（纯函数，可单测）：这次写入允许吗？
///
/// 两道防线（任一不过 → 拒绝，**绝不碰用户的文件**）：
/// 1. registry 真源：该 provider 是否声明 `configWritable: true`（只有验证过写结构的应用才为 true）
/// 2. 兜底：registry 没这个声明时，**只允许 json/json5 目标**
///    （YAML/TOML/SQLite/无扩展名 → 通用结构会把它写坏 → 宁可拒绝，让用户照教程手动配）
pub fn write_allowed(app_doc: Option<&Value>, provider: &str, target: &PathBuf) -> bool {
    if let Some(doc) = app_doc {
        // registry 有明确声明 → 以它为准（false 也不写）
        if let Some(b) = doc
            .pointer(&format!("/modelProviders/{provider}/configWritable"))
            .and_then(|x| x.as_bool())
        {
            return b;
        }
    }
    is_json_target(target)
}

/// 纯函数：app 文档里该 provider 是否声明为「AiBOX 知道怎么写」。
pub fn provider_writable_in(app_doc: &Value, provider: &str) -> bool {
    app_doc
        .pointer(&format!("/modelProviders/{provider}/configWritable"))
        .and_then(|x| x.as_bool())
        .unwrap_or(false)
}

/// 目标配置格式（仅用于错误说明；registry 的 `configFormat`）。
pub fn provider_format(input: &ConfigWriteInput) -> String {
    registry::read_cached_app(&input.app_id)
        .map(|v| provider_format_in(&v, &input.provider))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// 纯函数：app 文档里该 provider 的配置格式（仅用于错误说明）。
pub fn provider_format_in(app_doc: &Value, provider: &str) -> String {
    app_doc
        .pointer(&format!("/modelProviders/{provider}/configFormat"))
        .and_then(|x| x.as_str())
        .unwrap_or("unknown")
        .to_string()
}

/// 构造补丁（仅包含调用方提供的值）。
pub fn build_patch(input: &ConfigWriteInput) -> CoreResult<Value> {
    if !PROVIDERS.contains(&input.provider.as_str()) {
        return Err(CoreError::new(
            "CONFIG_BAD_PROVIDER",
            format!(
                "Unknown provider: {} (expected one of {})",
                input.provider,
                PROVIDERS.join("/")
            ),
        ));
    }
    let mut node = serde_json::Map::new();
    if let Some(k) = input.api_key.as_ref().filter(|k| !k.trim().is_empty()) {
        node.insert("apiKey".into(), json!(k.trim()));
    }
    if let Some(u) = input.base_url.as_ref().filter(|u| !u.trim().is_empty()) {
        node.insert("baseUrl".into(), json!(u.trim()));
    }
    if let Some(m) = input.model.as_ref().filter(|m| !m.trim().is_empty()) {
        node.insert("model".into(), json!(m.trim()));
    }
    if node.is_empty() {
        return Err(CoreError::new(
            "CONFIG_NO_VALUES",
            "Nothing to write: provide apiKey, baseUrl and/or model",
        ));
    }
    Ok(json!({
        "models": { "providers": { input.provider.clone(): Value::Object(node) } }
    }))
}

/// C5 全流程（同步；调用方放入 spawn_blocking）。
pub fn config_write(input: &ConfigWriteInput) -> CoreResult<ConfigWriteResult> {
    let app_doc = registry::read_cached_app(&input.app_id).ok();

    // ① 写入模板（registry 的 `configWrite`）—— 有模板就按**该应用真实结构**写。
    //    模板不合法 → 直接报错（fail-closed，不退回通用结构）。
    let template = match app_doc.as_ref() {
        Some(doc) => doc
            .pointer(&format!(
                "/modelProviders/{}/configWrite",
                input.provider
            ))
            .map(Template::from_value)
            .transpose()?,
        None => None,
    };

    // ② 模板存在 → 走模板写入（JSON/YAML/TOML/JSON5 通用）。
    if let Some(tpl) = template.as_ref() {
        // 模板也必须声明可写（registry 是真源；显式 false 一律不写）。
        if !provider_writable_in(
            app_doc.as_ref().expect("app doc present"),
            &input.provider,
        ) {
            return Err(CoreError::new(
                "CONFIG_FORMAT_UNSUPPORTED",
                format!(
                    "AiBOX does not know how to write this app's config ({} format); the file was left untouched",
                    tpl.format
                ),
            ));
        }
        let target = resolve_target_with_template(input, Some(tpl))?;
        let ctx = Ctx {
            provider: &input.provider,
            api_key: input.api_key.as_deref(),
            base_url: input.base_url.as_deref(),
            model: input.model.as_deref(),
        };
        let existing = if target.is_file() {
            std::fs::read_to_string(&target).map_err(|e| {
                CoreError::new(
                    "IO_ERROR",
                    format!("cannot read {}: {}", target.display(), e),
                )
            })?
        } else {
            String::new()
        };
        let (text, changed) = crate::config_tpl::render(tpl, &existing, &ctx)?;
        // 1) 备份（已有原文件才备份；首次写入 backupPath = ""）
        let backup_path = if target.is_file() {
            fsops::backup_file(&target)?.to_string_lossy().into_owned()
        } else {
            String::new()
        };
        // 2) 原子写（文本；多格式不能走 JSON 写入器）
        fsops::write_text_atomic(&target, &text)?;
        // 3) 回读校验（读盘内容必须与写入内容一致）
        let ok = fsops::readback_verify_text(&target, &text)?;
        return Ok(ConfigWriteResult {
            backup_path,
            ok,
            merged_keys: changed,
        });
    }

    // ③ 无模板 → 旧路径（通用 JSON5 结构）。
    //    **格式闸门**：只对「已知写结构」的应用动手。
    //    未知格式（yaml/toml/sqlite/…）→ 直接拒绝，**绝不碰用户的文件**。
    let patch = build_patch(input)?;
    let target = resolve_target(input)?;
    if !write_allowed(app_doc.as_ref(), &input.provider, &target) {
        return Err(CoreError::new(
            "CONFIG_FORMAT_UNSUPPORTED",
            format!(
                "AiBOX does not know how to write this app's config ({} format); the file was left untouched",
                provider_format(input)
            ),
        ));
    }

    // 1) 备份（已有原文件才备份；首次写入 backupPath = ""）
    let backup_path = if target.is_file() {
        fsops::backup_file(&target)?.to_string_lossy().into_owned()
    } else {
        String::new()
    };

    // 2) 读原文件（JSON5）→ 深合并保留未知键
    let mut base = fsops::read_json5(&target)?;
    let merged_keys = fsops::merge_preserve(&mut base, &patch);

    // 3) 原子写
    fsops::write_json_atomic(&target, &base)?;

    // 4) 回读校验
    let ok = fsops::readback_verify(&target, &base)?;

    Ok(ConfigWriteResult {
        backup_path,
        ok,
        merged_keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(target: PathBuf) -> ConfigWriteInput {
        ConfigWriteInput {
            app_id: "openclaw".into(),
            provider: "openrouter".into(),
            api_key: Some("sk-or-v1-abcdef0123456789".into()),
            base_url: None,
            model: Some("anthropic/claude-sonnet-4".into()),
            target_path: Some(target.to_string_lossy().into_owned()),
        }
    }

    #[test]
    fn patch_layout_and_provider_validation() {
        let p = build_patch(&ConfigWriteInput {
            provider: "anthropic".into(),
            api_key: Some(" sk-ant-xyz ".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            p.pointer("/models/providers/anthropic/apiKey").unwrap(),
            "sk-ant-xyz"
        );
        let bad = build_patch(&ConfigWriteInput {
            provider: "deepseek".into(),
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(bad.code, "CONFIG_BAD_PROVIDER");
        let empty = build_patch(&ConfigWriteInput {
            provider: "openai".into(),
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(empty.code, "CONFIG_NO_VALUES");
    }

    #[test]
    fn full_write_flow_preserves_keys_and_backs_up() {
        let d = std::env::temp_dir().join(format!(
            "aibox-config-{}-{}",
            std::process::id(),
            fsops::now_ts()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let target = d.join("openclaw.json5");
        std::fs::write(
            &target,
            r#"{
  // user's own comment must survive? (comments are normalized away on write)
  "models": { "mode": "manual" },
  "custom": { "keep": "me" }
}"#,
        )
        .unwrap();

        let res = config_write(&input(target.clone())).unwrap();
        assert!(res.ok);
        assert!(
            res.backup_path.contains("openclaw.json5.bak."),
            "{}",
            res.backup_path
        );
        assert!(res
            .merged_keys
            .contains(&"models.providers.openrouter.apiKey".to_string()));
        assert!(res
            .merged_keys
            .contains(&"models.providers.openrouter.model".to_string()));
        // 未知键保留
        let v = fsops::read_json5(&target).unwrap();
        assert_eq!(v["models"]["mode"], "manual");
        assert_eq!(v["custom"]["keep"], "me");
        // 备份保留原内容
        let bak = std::path::PathBuf::from(&res.backup_path);
        assert!(std::fs::read_to_string(&bak)
            .unwrap()
            .contains("user's own comment"));
        // merged_keys 不含任何 key 值
        for k in &res.merged_keys {
            assert!(!k.contains("sk-or-v1"));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn first_write_without_existing_file_succeeds() {
        let d = std::env::temp_dir().join(format!(
            "aibox-config2-{}-{}",
            std::process::id(),
            fsops::now_ts()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let target = d.join("fresh.json5");
        let res = config_write(&input(target.clone())).unwrap();
        assert!(res.ok);
        assert_eq!(res.backup_path, "");
        assert!(target.is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_target_prefers_explicit_path() {
        let r = resolve_target(&ConfigWriteInput {
            target_path: Some("~/x/y.json5".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r, paths::expand("~/x/y.json5"));
        // 无 appId 无 target → 结构化错误
        let e = resolve_target(&ConfigWriteInput::default()).unwrap_err();
        assert_eq!(e.code, "CONFIG_NO_APP");
    }
}

#[cfg(test)]
mod write_gate_tests {
    use super::*;
    use serde_json::json;

    fn doc(writable: Option<bool>) -> Value {
        let mut mp = serde_json::Map::new();
        let mut p = serde_json::Map::new();
        p.insert("configFormat".into(), json!("yaml"));
        p.insert("configTarget".into(), json!("~/.aider.conf.yml"));
        if let Some(w) = writable {
            p.insert("configWritable".into(), json!(w));
        }
        mp.insert("openai".into(), Value::Object(p));
        json!({ "modelProviders": Value::Object(mp) })
    }

    #[test]
    fn registry_declares_writable_is_the_truth() {
        let t = PathBuf::from("/tmp/x.json5");
        // 明确声明可写 → 允许（即使格式看着陌生，registry 是真源）
        assert!(write_allowed(Some(&doc(Some(true))), "openai", &t));
        // 明确声明不可写 → 拒绝（哪怕目标是 .json）
        assert!(!write_allowed(
            Some(&doc(Some(false))),
            "openai",
            &PathBuf::from("/tmp/x.json")
        ));
    }

    #[test]
    fn no_registry_declaration_falls_back_to_json_only() {
        // 没有声明 → 只允许 json/json5（YAML/TOML/SQLite 绝不能碰）
        assert!(write_allowed(None, "openai", &PathBuf::from("/tmp/x.json5")));
        assert!(write_allowed(None, "openai", &PathBuf::from("/tmp/x.json")));
        assert!(!write_allowed(None, "openai", &PathBuf::from("/tmp/x.yml")));
        assert!(!write_allowed(None, "openai", &PathBuf::from("/tmp/x.toml")));
        assert!(!write_allowed(None, "openai", &PathBuf::from("/tmp/x.db")));
        assert!(!write_allowed(None, "openai", &PathBuf::from("/tmp/ComfyUI")));
        // 有声明但没这个 provider → 同样退回 json 兜底
        assert!(!write_allowed(
            Some(&doc(Some(true))),
            "anthropic",
            &PathBuf::from("/tmp/x.yml")
        ));
    }
}
