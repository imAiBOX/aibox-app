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
    let patch = build_patch(input)?;
    let target = resolve_target(input)?;

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
