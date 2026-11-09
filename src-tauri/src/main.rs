#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! AiBOX portable 壳：空窗口 + 品牌页。状态机 / NetLayer 接入在 。

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![channel_types])
        .run(tauri::generate_context!())
        .expect("error while running AiBOX");
}

/// 壳 ↔ aibox-net 的最小链路（net 层独立可测，壳只做转发）。
/// 的通道徽章/切换 toast 会用到。
#[tauri::command]
fn channel_types() -> Vec<&'static str> {
    aibox_net::ChannelType::all()
        .iter()
        .map(|c| c.as_str())
        .collect()
}
