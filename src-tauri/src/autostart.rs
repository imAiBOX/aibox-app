//! 开机自启（tauri-plugin-autostart，官方插件）。
//!
//! - Windows = HKCU `Run` 键（免提权，全程 0 提权）；macOS = LaunchAgent；Linux = autostart .desktop
//! - **默认开启**：首次启动（或上次注册失败）时尝试注册；失败不阻塞启动，
//!   设置页显示人话提示（可能被安全软件/策略阻止）。
//! - 状态标记落 `~/.aibox/autostart.json`（区分「用户主动关闭」与「从未设置」）。

use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_autostart::ManagerExt;

use aibox_core::paths;

/// 自启状态（契约：autostart_get / autostart_set 返回体）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AutostartState {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn flag_file() -> std::path::PathBuf {
    paths::aibox_root().join("autostart.json")
}

fn read_flag() -> Option<AutostartState> {
    std::fs::read_to_string(flag_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn write_flag(s: &AutostartState) {
    let _ = std::fs::create_dir_all(paths::aibox_root());
    if let Ok(text) = serde_json::to_string_pretty(s) {
        let _ = std::fs::write(flag_file(), text);
    }
}

/// OS 侧真实状态（HKCU Run 键 / LaunchAgent 是否存在）。
fn os_enabled(app: &AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

/// 首次启动默认注册；上次失败则每次启动重试（成功后不再自动改动）。
pub fn ensure_default(app: &AppHandle) {
    let retry = match read_flag() {
        None => true,        // 从未设置 → 默认开（所有者拍板）
        Some(f) => f.error.is_some(), // 上次被拦 → 重试
    };
    if !retry {
        return;
    }
    match app.autolaunch().enable() {
        Ok(()) => write_flag(&AutostartState {
            enabled: true,
            error: None,
        }),
        Err(e) => write_flag(&AutostartState {
            enabled: false,
            error: Some(e.to_string()),
        }),
    }
}

/// 当前状态（enabled 取 OS 真相；error 取最近一次失败记录）。
pub fn status(app: &AppHandle) -> AutostartState {
    let flag = read_flag().unwrap_or_default();
    let enabled = os_enabled(app);
    AutostartState {
        enabled,
        error: if enabled { None } else { flag.error },
    }
}

/// 设置开关（即时生效）；失败时返回 OS 真实状态 + 错误（设置页人话提示）。
pub fn set(app: &AppHandle, enabled: bool) -> AutostartState {
    let result = if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    match result {
        Ok(()) => {
            let s = AutostartState {
                enabled,
                error: None,
            };
            write_flag(&s);
            s
        }
        Err(e) => {
            let s = AutostartState {
                enabled: os_enabled(app),
                error: Some(e.to_string()),
            };
            write_flag(&s);
            s
        }
    }
}
