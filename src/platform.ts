/**
 * 运行平台判定（前端侧，同步）：
 * Tauri WebView 在 macOS 上 navigator.platform 为 MacIntel / macOS，
 * userAgent 含 "Mac OS X"。用于按平台切换引导文案（SmartScreen vs Gatekeeper）。
 */
export function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  const platform = navigator.platform || "";
  const ua = navigator.userAgent || "";
  return /^mac/i.test(platform) || /Mac OS X/.test(ua);
}
