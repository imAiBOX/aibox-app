//! 系统托盘（Windows / macOS 通用）。
//!
//! - 左键单击/双击 → 显示主窗口；右键 → 菜单（首项动态 / 检查更新 / 退出）
//! - 「退出」= 真正结束进程；安装进行中先发 `tray_action{quit}`，由前端弹确认层
//! - 托盘文案由前端 i18n 后经 `tray_sync` 下发（含 tooltip 与首项动作）

use std::sync::{LazyLock, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

/// 前端下发的托盘文案（已 i18n 的最终字符串）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayUi {
    pub tooltip: String,
    pub first_label: String,
    /// `open` = 显示主窗口；`download` = 打开官网下载页（经前端 window.open）
    pub first_action: String,
    pub check_label: String,
    pub quit_label: String,
}

impl Default for TrayUi {
    fn default() -> Self {
        // TS 尚未加载时的英文兜底（前端启动后立即 tray_sync 覆盖为当前语言）
        Self {
            tooltip: "AiBOX".into(),
            first_label: "Open AiBOX".into(),
            first_action: "open".into(),
            check_label: "Check for updates".into(),
            quit_label: "Quit".into(),
        }
    }
}

/// 运行期持有的托盘句柄（更新文案用）。
pub struct TrayHandles {
    pub tray: TrayIcon<Wry>,
    pub first: MenuItem<Wry>,
    pub check: MenuItem<Wry>,
    pub quit: MenuItem<Wry>,
}

static HANDLES: LazyLock<Mutex<Option<TrayHandles>>> = LazyLock::new(|| Mutex::new(None));

fn handles() -> MutexGuard<'static, Option<TrayHandles>> {
    HANDLES.lock().unwrap_or_else(|p| p.into_inner())
}

/// 显示并聚焦主窗口（托盘左键/首项「打开」用）。
pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn on_first(app: &AppHandle) {
    let download = crate::state::tray_ui().first_action == "download";
    if download {
        let _ = app.emit(
            "tray_action",
            serde_json::json!({ "action": "download" }),
        );
    } else {
        show_main(app);
    }
}

/// 构建托盘（失败由调用方记录日志，不阻塞启动）。
pub fn init(app: &AppHandle) -> tauri::Result<()> {
    let first = MenuItem::with_id(app, "first", "Open AiBOX", true, None::<&str>)?;
    let check = MenuItem::with_id(app, "check", "Check for updates", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&first, &check, &sep, &quit])?;

    // 标准 logo（RGBA png；32/128 多尺寸由系统按托盘缩放取样）
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/icon.png"))?;

    let tray = TrayIconBuilder::with_id("aibox-tray")
        .icon(icon)
        .tooltip("AiBOX")
        // 左键 = 显示窗口（菜单只走右键；macOS 右键菜单为 tray-icon 默认行为）
        .show_menu_on_left_click(false)
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "first" => on_first(app),
            "check" => {
                show_main(app);
                let _ = app.emit("tray_action", serde_json::json!({ "action": "check" }));
            }
            "quit" => {
                show_main(app);
                let _ = app.emit("tray_action", serde_json::json!({ "action": "quit" }));
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| match event {
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            }
            | TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            } => show_main(tray.app_handle()),
            _ => {}
        })
        .build(app)?;

    *handles() = Some(TrayHandles {
        tray,
        first: first.clone(),
        check: check.clone(),
        quit,
    });
    Ok(())
}

/// 应用前端下发的文案（tooltip + 菜单项文本）。
pub fn apply_ui(ui: &TrayUi) {
    let guard = handles();
    let Some(h) = guard.as_ref() else {
        return;
    };
    let _ = h.tray.set_tooltip(Some(ui.tooltip.as_str()));
    let _ = h.first.set_text(ui.first_label.as_str());
    let _ = h.check.set_text(ui.check_label.as_str());
    let _ = h.quit.set_text(ui.quit_label.as_str());
}
