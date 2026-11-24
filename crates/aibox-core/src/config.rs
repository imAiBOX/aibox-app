//! The desktop config command accepts only a supported, network-checked catalog template.

#[cfg(any(target_os = "macos", test))]
use std::{path::Path, sync::Mutex};

use serde::Serialize;
#[cfg(any(target_os = "macos", test))]
use serde_json::Value;

#[cfg(any(target_os = "macos", test))]
use crate::config_tpl::{Ctx, Template};
use crate::err::{CoreError, CoreResult};
#[cfg(target_os = "macos")]
use crate::paths;
#[cfg(any(target_os = "macos", test))]
use crate::registry;

#[cfg(any(target_os = "macos", all(test, target_os = "linux")))]
#[path = "config_fs.rs"]
mod config_fs;

#[cfg(all(test, not(any(target_os = "macos", target_os = "linux"))))]
mod config_fs {
    use super::*;

    pub(super) fn write(
        _home: &Path,
        _relative: &str,
        _render: impl FnOnce(&str) -> CoreResult<(String, Vec<String>)>,
    ) -> CoreResult<ConfigWriteResult> {
        Err(unavailable())
    }
}

#[cfg(not(target_os = "macos"))]
fn unavailable() -> CoreError {
    // Do not fall back to Win32 path-based I/O or remove-then-rename. A
    // verified catalog entry is not a guarantee of safe local I/O.
    CoreError::new(
        "CONFIG_FORMAT_UNSUPPORTED",
        "Safe automatic configuration is not available on this operating system; configure the app manually",
    )
}

#[derive(Debug, Clone, Default)]
pub struct ConfigWriteInput {
    pub app_id: String,
    pub provider: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigWriteResult {
    /// Empty when a file was created for the first time.
    pub backup_path: String,
    pub ok: bool,
    pub merged_keys: Vec<String>,
}

fn unsupported() -> CoreError {
    CoreError::new(
        "CONFIG_FORMAT_UNSUPPORTED",
        "No supported configuration template for this app, provider and platform",
    )
}

/// Native enforcement for config-capability-v1. Fixtures shared with the UI and MCP cover the catalog decisions.
#[cfg(any(target_os = "macos", test))]
fn select_template<'a>(
    profile: &'a Value,
    input: &ConfigWriteInput,
    platform: &str,
) -> CoreResult<(Template, &'a str)> {
    let has_package = profile
        .get("packages")
        .and_then(Value::as_array)
        .is_some_and(|packages| {
            packages.iter().any(|pkg| {
                pkg.get("kind").and_then(Value::as_str) == Some("app")
                    && pkg
                        .get("platforms")
                        .and_then(Value::as_array)
                        .is_some_and(|platforms| {
                            platforms.iter().any(|os| os.as_str() == Some(platform))
                        })
            })
        });
    if !matches!(platform, "windows" | "macos")
        || !has_package
        || profile.get("id").and_then(Value::as_str) != Some(input.app_id.as_str())
        || !matches!(
            input.provider.as_str(),
            "openrouter" | "anthropic" | "openai"
        )
    {
        return Err(unsupported());
    }
    let provider = profile
        .get("modelProviders")
        .and_then(|p| p.get(&input.provider))
        .ok_or_else(unsupported)?;
    if provider.get("authType").and_then(Value::as_str) != Some("api-key")
        || provider.get("configWritable").and_then(Value::as_bool) != Some(true)
    {
        return Err(unsupported());
    }
    let raw = provider.get("configWrite").ok_or_else(unsupported)?;
    let template = Template::from_value(raw).map_err(|_| unsupported())?;
    if !template.supports_assisted_key() {
        return Err(unsupported());
    }
    let target = raw
        .get("file")
        .or_else(|| provider.get("configTarget"))
        .and_then(Value::as_str)
        .ok_or_else(unsupported)?;
    let relative = target.strip_prefix("~/").ok_or_else(unsupported)?;
    if target.trim() != target
        || target
            .chars()
            .any(|ch| matches!(ch, '\\' | ':' | '%' | '<' | '>' | '\u{0}'..='\u{1f}' | '\u{7f}'))
        || relative
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        || (platform == "macos"
            && relative
                .split('/')
                .next()
                .is_some_and(|first| first.eq_ignore_ascii_case("appdata")))
    {
        return Err(unsupported());
    }
    Ok((template, relative))
}

// Serialize the local read/backup/write transaction across desktop commands.
#[cfg(any(target_os = "macos", test))]
static CONFIG_WRITES: Mutex<()> = Mutex::new(());

/// Old clients may omit this argument or send an empty string, but never select a target.
pub fn reject_legacy_target(target: Option<&str>) -> CoreResult<()> {
    if target.is_some_and(|value| !value.is_empty()) {
        return Err(unsupported());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn config_write(_input: &ConfigWriteInput) -> CoreResult<ConfigWriteResult> {
    Err(unavailable())
}

#[cfg(target_os = "macos")]
pub fn config_write(input: &ConfigWriteInput) -> CoreResult<ConfigWriteResult> {
    config_write_with(input, "macos", &paths::home_dir(), |id| {
        registry::fetch_app_network_verified(id).map_err(|e| {
            if registry::is_availability_err(&e) {
                catalog_unverified()
            } else {
                e
            }
        })
    })
}

#[cfg(any(target_os = "macos", test))]
fn catalog_unverified() -> CoreError {
    CoreError::new(
        "CONFIG_CATALOG_UNVERIFIED",
        "Reconnect to verify the app's configuration instructions",
    )
}

/// Shared command implementation. The caller cannot supply an OS, a write target or a catalog.
#[cfg(any(target_os = "macos", test))]
fn config_write_with(
    input: &ConfigWriteInput,
    platform: &str,
    home: &Path,
    read_profile: impl FnOnce(&str) -> CoreResult<(Value, String)>,
) -> CoreResult<ConfigWriteResult> {
    if !matches!(platform, "windows" | "macos")
        || !matches!(
            input.provider.as_str(),
            "openrouter" | "anthropic" | "openai"
        )
    {
        return Err(unsupported());
    }
    registry::validate_id(&input.app_id)?;
    let (profile, source) = read_profile(&input.app_id)?;
    if !registry::is_network_verified_source(&source) {
        return Err(catalog_unverified());
    }
    let (template, relative) = select_template(&profile, input, platform)?;
    let key = input
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .ok_or_else(|| CoreError::new("CONFIG_NO_VALUES", "An API key is required"))?;
    let prepared = template.prepare_assisted(&Ctx {
        provider: &input.provider,
        api_key: Some(key),
        base_url: input.base_url.as_deref(),
        model: input.model.as_deref(),
    })?;
    let _guard = CONFIG_WRITES
        .lock()
        .map_err(|_| CoreError::new("CONFIG_FAILED", "Cannot acquire configuration write lock"))?;
    config_fs::write(home, relative, |old| prepared.render(old))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fsops;

    fn fixtures() -> Value {
        serde_json::from_str(include_str!(
            "../../../src/config/config-capability-v1.fixtures.json"
        ))
        .unwrap()
    }

    #[test]
    fn config_write_entry_enforces_shared_platform_fixtures() {
        let fixture = fixtures();
        // macOS may expose its test temp root under /var, which is a symlink.
        // Only the test harness normalizes it; production rejects symlink components.
        let home = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "aibox-config-v1-{}-{}",
            std::process::id(),
            fsops::now_ts()
        ));
        std::fs::create_dir(&home).unwrap();
        for (index, case) in fixture["cases"].as_array().unwrap().iter().enumerate() {
            let app_id = case["appId"].as_str().unwrap();
            let platform = case["platform"].as_str().unwrap();
            let input = ConfigWriteInput {
                app_id: app_id.into(),
                provider: case["provider"].as_str().unwrap().into(),
                api_key: Some("sk-fixture-key".into()),
                ..Default::default()
            };
            let case_home = home.join(index.to_string());
            std::fs::create_dir(&case_home).unwrap();
            // The injected writer is a macOS harness; the real Windows entry is cfg-disabled.
            if platform == "windows" {
                assert_ne!(case["expected"], "assisted", "{case}");
                #[cfg(not(target_os = "macos"))]
                assert_eq!(
                    config_write(&input).unwrap_err().code,
                    "CONFIG_FORMAT_UNSUPPORTED",
                    "{case}"
                );
                assert!(std::fs::read_dir(case_home).unwrap().next().is_none());
                continue;
            }
            let profile = &fixture["profiles"][app_id];
            let read_profile = |_: &str| Ok((profile.clone(), "raw".into()));
            let result = config_write_with(&input, platform, &case_home, read_profile);
            if case["expected"] == "assisted" {
                assert_eq!(platform, "macos");
                let relative = select_template(profile, &input, platform).unwrap().1;
                if !cfg!(any(target_os = "macos", target_os = "linux")) {
                    assert_eq!(result.unwrap_err().code, "CONFIG_FORMAT_UNSUPPORTED");
                    assert!(std::fs::read_dir(case_home).unwrap().next().is_none());
                    continue;
                }
                let target = case_home.join(relative);
                let first_write = result.unwrap_or_else(|e| panic!("{case}: {e:?}"));
                assert!(std::fs::read_to_string(&target)
                    .unwrap()
                    .contains("sk-fixture-key"));
                assert!(first_write.ok && first_write.backup_path.is_empty());
                let previous = std::fs::read(&target).unwrap();
                let next_write =
                    config_write_with(&input, platform, &case_home, read_profile).unwrap();
                assert!(next_write.ok);
                assert_eq!(std::fs::read(next_write.backup_path).unwrap(), previous);
            } else {
                assert_eq!(
                    result.unwrap_err().code,
                    "CONFIG_FORMAT_UNSUPPORTED",
                    "{case}"
                );
                assert!(std::fs::read_dir(case_home).unwrap().next().is_none());
            }
        }
        std::fs::remove_dir_all(home).unwrap();
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn desktop_write_is_disabled_without_a_supported_backend() {
        let err = config_write(&ConfigWriteInput::default()).unwrap_err();
        assert_eq!(err.code, "CONFIG_FORMAT_UNSUPPORTED");
        assert!(err.human.contains("configure the app manually"));
    }

    #[test]
    fn legacy_target_is_rejected_before_any_side_effect() {
        assert_eq!(
            reject_legacy_target(Some("/tmp/attacker.json"))
                .unwrap_err()
                .code,
            "CONFIG_FORMAT_UNSUPPORTED"
        );
        assert!(reject_legacy_target(None).is_ok());
        assert!(reject_legacy_target(Some("")).is_ok());
    }

    #[test]
    fn config_write_rejects_nonempty_catalog_file_placeholder() {
        let mut profile = fixtures()["profiles"]["chatbox"].clone();
        profile["modelProviders"]["openai"]["configWrite"]["file"] =
            serde_json::json!("~/AppData/<apiKey>/config.json");
        let input = ConfigWriteInput {
            app_id: "chatbox".into(),
            provider: "openai".into(),
            api_key: Some("sk-test".into()),
            ..Default::default()
        };
        let home =
            std::env::temp_dir().join(format!("aibox-config-placeholder-{}", fsops::now_ts()));
        std::fs::create_dir_all(&home).unwrap();
        let err = config_write_with(&input, "windows", &home, |_| Ok((profile, "raw".into())))
            .unwrap_err();
        assert_eq!(err.code, "CONFIG_FORMAT_UNSUPPORTED");
        assert!(std::fs::read_dir(&home).unwrap().next().is_none());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn offline_catalog_does_not_enable_automatic_setup() {
        let profile = fixtures()["profiles"]["chatbox"].clone();
        let input = ConfigWriteInput {
            app_id: "chatbox".into(),
            provider: "openai".into(),
            api_key: Some("sk-fixture-key".into()),
            ..Default::default()
        };
        let err = config_write_with(&input, "windows", Path::new("."), |_| {
            Ok((profile, "cache-stale".into()))
        })
        .unwrap_err();
        assert_eq!(err.code, "CONFIG_CATALOG_UNVERIFIED");
    }
}
