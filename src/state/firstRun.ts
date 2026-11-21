/**
 * 首次运行标记（持久化）。
 *
 * **为什么不放 KV**：首启引导是一次性的轻量标记，localStorage 足够，
 * 少一次异步存储往返 = 引导能立刻出现（用户点开软件第一眼就该看到，而不是空白）。
 */

const KEY = "aibox.firstRun.seen";

/** 是否已经看过首启引导 */
export function hasSeenFirstRun(): boolean {
  try {
    if (typeof localStorage === "undefined") return false;
    return localStorage.getItem(KEY) === "1";
  } catch {
    return false;
  }
}

/** 标记首启引导已看过（下次不再打扰） */
export function markFirstRunSeen(): void {
  try {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(KEY, "1");
  } catch {
    /* 隐私模式 → 下次再看（可接受） */
  }
}

/** 是否**第一次**启动（还没看过引导）→ 首屏不打扰（不弹更新条）。 */
export function isFirstLaunch(): boolean {
  return !hasSeenFirstRun();
}
