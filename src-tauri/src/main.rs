#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! AiBOX 壳（全量 command + 事件 + 插件注册）。
//!
//! 业务编排在 TS 侧；Rust 只做稳定原语的薄封装（`aibox-core` 可脱离 Tauri 单测）。
//! tauri-plugin-updater 不装（黄条方案：只做 `update_check`）。

mod commands;
mod error;
mod state;

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
            commands::diag_export,
            commands::update_check,
            commands::locales_load,
            // 保留
            commands::channel_types,
        ])
        .run(tauri::generate_context!())
        .expect("error while running AiBOX");
}
