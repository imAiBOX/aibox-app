/**
 * 外链统一走系统浏览器。Tauri webview 内 `window.open` 无效（这就是「黄条下载点了没反应」的根因），
 * 必须经 tauri-plugin-opener；非 Tauri 环境（浏览器 dev / vitest）回落 window.open。
 */
import { openUrl } from "@tauri-apps/plugin-opener";

export async function openExternal(url: string): Promise<void> {
  if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
    try {
      await openUrl(url);
      return;
    } catch (e) {
      console.warn("[openExternal] opener failed, fallback to window.open:", e);
    }
  }
  window.open(url, "_blank", "noopener,noreferrer");
}
