#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! AiBOX 壳（全量 command + 事件 + 插件注册）。
//!
//! 业务编排在 TS 侧；Rust 只做稳定原语的薄封装（`aibox-core` 可脱离 Tauri 单测）。
//! tauri-plugin-updater 不装（黄条方案：只做 `update_check`）。

mod autostart;
mod commands;
mod error;
mod state;
mod tray;

fn main() {
    // 下载取消走 panic 短路（见 commands::download_start）：静默该标记的 panic
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        if msg.contains("aibox-download-cancelled") {
            return;
        }
        default_hook(info);
    }));

    tauri::Builder::default()
        // SQLite（断点续装）：只注册，表结构与读写在 TS 侧
        .plugin(tauri_plugin_sql::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        // 开机自启（官方插件；Windows = HKCU Run 键，免提权）
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        // 窗口 X = 隐藏到托盘（不退出）；退出只走托盘菜单
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.label() == "main" {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            // 系统与注册表
            commands::scan_system,
            commands::registry_index,
            commands::registry_app,
            // 网络（net 层）
            commands::net_region,
            commands::net_region_set,
            commands::net_probe,
            commands::netdoctor_run,
            // 执行任务（EXECUTE 四阶段）
            commands::download_start,
            commands::download_cancel,
            commands::verify_sha256,
            commands::exec_install,
            commands::probe_run,
            // CONFIG / VERIFY
            commands::config_write,
            commands::verify_send,
            // 诊断与更新
            commands::save_plan,
            commands::diag_export,
            commands::update_check,
            commands::locales_load,
            // 托盘 / 自启 / 应用更新
            commands::check_app_updates,
            commands::install_record,
            // MANAGE 维护模式
            commands::manage_backup,
            commands::manage_remove,
            commands::manage_manifest_list,
            commands::tray_sync,
            commands::app_quit,
            commands::autostart_get,
            commands::autostart_set,
            // 保留
            commands::channel_types,
        ])
        .setup(|app| {
            // 托盘/自启失败不阻塞启动（仅记日志；自启失败可在设置页重试）
            if let Err(e) = tray::init(app.handle()) {
                eprintln!("[aibox] tray init failed: {e}");
            }
            autostart::ensure_default(app.handle());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running AiBOX");
}
