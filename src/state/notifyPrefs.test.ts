/**
 * 通知偏好与「同一事件只提醒一次」的去重逻辑（体验克制的核心）。
 */
import { describe, expect, it } from "vitest";
import { DEFAULT_NOTIFY_PREFS, createNotifyStore } from "./notifyPrefs";

describe("通知偏好", () => {
  it("默认：三类通知都开；「启动即收进托盘」默认关（不擅自改变用户预期）", () => {
    expect(DEFAULT_NOTIFY_PREFS.coreUpdate).toBe(true);
    expect(DEFAULT_NOTIFY_PREFS.appUpdate).toBe(true);
    expect(DEFAULT_NOTIFY_PREFS.newApps).toBe(true);
    expect(DEFAULT_NOTIFY_PREFS.startMinimized).toBe(false);
    expect(DEFAULT_NOTIFY_PREFS.minimizeAfterInstall).toBe(true);
  });

  it("保存后能读回（内存后端）", async () => {
    const store = createNotifyStore(null);
    const p = { ...DEFAULT_NOTIFY_PREFS, newApps: false };
    await store.savePrefs(p);
    const loaded = await store.loadPrefs();
    expect(loaded.newApps).toBe(false);
    expect(loaded.appUpdate).toBe(true);
  });

  it("markSeen：同一 key 只第一次返回 true（不重复打扰）", async () => {
    const store = createNotifyStore(null);
    expect(await store.markSeen("upd:codex@1.0")).toBe(true);
    expect(await store.markSeen("upd:codex@1.0")).toBe(false);
    // 不同版本 → 视为新事件
    expect(await store.markSeen("upd:codex@1.1")).toBe(true);
  });
});
