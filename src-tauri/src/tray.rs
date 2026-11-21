//! 系统托盘（Windows / macOS 通用）。
//!
//! - 左键单击/双击 → 显示主窗口；右键 → 菜单（待办项 / 检查更新 / 退出）
//! - **托盘是「状态面板」**：有待办（已装应用可升级 / 目录上新 / 本体新版）时，
//!   菜单顶部列出待办并给图标加角标数字（Windows）；窗口关着也能一眼看到「有事要做」
//! - 「退出」= 真正结束进程；安装进行中先发 `tray_action{quit}`，由前端弹确认层
//! - 托盘文案由前端 i18n 后经 `tray_sync` 下发（含 tooltip 与首项动作）

use std::sync::{LazyLock, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
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
    /// 角标数字（Windows 托盘图标旁；空 = 不显示）。非 Windows 平台忽略。
    #[serde(default)]
    pub badge: String,
    /// 已装应用可升级的菜单项（count=0 → 隐藏）
    #[serde(default)]
    pub pending_updates_label: String,
    #[serde(default)]
    pub pending_updates_count: u32,
    /// 目录新上的菜单项（count=0 → 隐藏）
    #[serde(default)]
    pub pending_new_label: String,
    #[serde(default)]
    pub pending_new_count: u32,
    /// 本体有新版的菜单项（count=0 → 隐藏）
    #[serde(default)]
    pub pending_core_label: String,
    #[serde(default)]
    pub pending_core_count: u32,
    /// 最近安装的应用（托盘右键直接装，不用进主界面）
    #[serde(default)]
    pub recent: Vec<RecentItem>,
}

/// 托盘「最近使用」项（id 用于点击回传；label 已是最终文案）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentItem {
    pub id: String,
    pub label: String,
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
            badge: String::new(),
            pending_updates_label: String::new(),
            pending_updates_count: 0,
            pending_new_label: String::new(),
            pending_new_count: 0,
            pending_core_label: String::new(),
            pending_core_count: 0,
            recent: Vec::new(),
        }
    }
}

/// 运行期持有的托盘句柄（更新文案用）。
#[derive(Clone)]
pub struct TrayHandles {
    pub tray: TrayIcon<Wry>,
    pub first: MenuItem<Wry>,
    pub check: MenuItem<Wry>,
    pub quit: MenuItem<Wry>,
    /// 已装应用可升级（待办项）
    pub pending_updates: MenuItem<Wry>,
    /// 目录新上（待办项）
    pub pending_new: MenuItem<Wry>,
    /// 本体有新版（待办项）
    pub pending_core: MenuItem<Wry>,
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
    // 待办项（默认隐藏；有内容才显示）
    let pending_updates = MenuItem::with_id(app, "pending_updates", "", true, None::<&str>)?;
    let pending_new = MenuItem::with_id(app, "pending_new", "", true, None::<&str>)?;
    let pending_core = MenuItem::with_id(app, "pending_core", "", true, None::<&str>)?;
    let check = MenuItem::with_id(app, "check", "Check for updates", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    // 初始菜单只放「打开 / 检查更新 / 退出」；待办项由 apply_ui 按需重建加入
    // （该 tauri 版本的 MenuItem 无 set_visible → 用重建菜单实现显隐）。
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
            // 待办项 → 打开窗口并告诉前端跳到哪（前端负责聚焦对应界面）
            // 「最近使用」→ 直接开始装那个应用（不进主界面）
            id if id.starts_with("recent:") => {
                show_main(app);
                let app_id = id.trim_start_matches("recent:").to_string();
                let _ = app.emit(
                    "tray_action",
                    serde_json::json!({ "action": format!("install:{app_id}") }),
                );
            }
            "pending_updates" | "pending_new" | "pending_core" => {
                show_main(app);
                let action = match event.id().as_ref() {
                    "pending_updates" => "updates",
                    "pending_new" => "newapps",
                    _ => "core",
                };
                let _ = app.emit("tray_action", serde_json::json!({ "action": action }));
            }
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
        pending_updates: pending_updates.clone(),
        pending_new: pending_new.clone(),
        pending_core: pending_core.clone(),
    });
    Ok(())
}

/// 应用前端下发的文案（tooltip + 菜单项文本 + 角标 + 待办项显隐）。
pub fn apply_ui(ui: &TrayUi) {
    let snapshot = handles().clone();
    let Some(h) = snapshot else {
        return;
    };
    let _ = h.tray.set_tooltip(Some(ui.tooltip.as_str()));
    let _ = h.first.set_text(ui.first_label.as_str());
    let _ = h.check.set_text(ui.check_label.as_str());
    let _ = h.quit.set_text(ui.quit_label.as_str());
    // Windows 托盘角标（其他平台为 no-op）
    let _ = h.tray.set_title(if ui.badge.is_empty() {
        None
    } else {
        Some(ui.badge.as_str())
    });
    // 待办项：count>0 才进菜单（否则不留空行）
    let _ = h
        .pending_updates
        .set_text(ui.pending_updates_label.as_str());
    let _ = h.pending_new.set_text(ui.pending_new_label.as_str());
    let _ = h.pending_core.set_text(ui.pending_core_label.as_str());
    rebuild_menu(&h, ui);
}

/// 按待办重建菜单，复用已有句柄，避免在调用原生菜单 API 时持锁。
fn rebuild_menu(h: &TrayHandles, ui: &TrayUi) {
    let app = h.tray.app_handle();
    let first: &dyn IsMenuItem<Wry> = &h.first;
    let upd: &dyn IsMenuItem<Wry> = &h.pending_updates;
    let new_it: &dyn IsMenuItem<Wry> = &h.pending_new;
    let core: &dyn IsMenuItem<Wry> = &h.pending_core;
    let check: &dyn IsMenuItem<Wry> = &h.check;
    let quit: &dyn IsMenuItem<Wry> = &h.quit;
    let mut items: Vec<&dyn IsMenuItem<Wry>> = vec![first];
    if ui.pending_updates_count > 0 {
        items.push(upd);
    }
    if ui.pending_new_count > 0 {
        items.push(new_it);
    }
    if ui.pending_core_count > 0 {
        items.push(core);
    }
    // 最近使用（最多 3 个）：菜单项要先存住再取引用（否则借用 outlive 报错）
    let mut stored: Vec<MenuItem<Wry>> = Vec::new();
    for r in ui.recent.iter().take(3) {
        if let Ok(item) = MenuItem::with_id(
            app,
            format!("recent:{}", r.id),
            r.label.clone(),
            true,
            None::<&str>,
        ) {
            stored.push(item);
        }
    }
    for item in &stored {
        let it: &dyn IsMenuItem<Wry> = item;
        items.push(it);
    }
    items.push(check);
    items.push(quit);
    if let Ok(menu) = Menu::with_items(app, &items) {
        let _ = h.tray.set_menu(Some(menu));
    }
}

