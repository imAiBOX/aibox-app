/**
 * 通知偏好与「同一事件只提醒一次」的去重逻辑（体验克制的核心）。
 */
import { describe, expect, it } from "vitest";
import { DEFAULT_NOTIFY_PREFS, createNotifyStore, isQuietNow } from "./notifyPrefs";

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

describe("免打扰时段", () => {
  const at = (h: number) => new Date(2026, 0, 1, h, 0, 0);

  it("关闭 → 永不静默", () => {
    expect(
      isQuietNow({ quietEnabled: false, quietStart: 22, quietEnd: 8 }, at(23)),
    ).toBe(false);
  });

  it("跨零点：22:00-08:00（22/23/0-7 静默，8-21 不静默）", () => {
    const q = { quietEnabled: true, quietStart: 22, quietEnd: 8 };
    expect(isQuietNow(q, at(22))).toBe(true);
    expect(isQuietNow(q, at(23))).toBe(true);
    expect(isQuietNow(q, at(0))).toBe(true);
    expect(isQuietNow(q, at(7))).toBe(true);
    expect(isQuietNow(q, at(8))).toBe(false);
    expect(isQuietNow(q, at(12))).toBe(false);
    expect(isQuietNow(q, at(21))).toBe(false);
  });

  it("不跨零点：09:00-18:00", () => {
    const q = { quietEnabled: true, quietStart: 9, quietEnd: 18 };
    expect(isQuietNow(q, at(9))).toBe(true);
    expect(isQuietNow(q, at(17))).toBe(true);
    expect(isQuietNow(q, at(18))).toBe(false);
    expect(isQuietNow(q, at(8))).toBe(false);
  });

  it("起止相同 → 视为未设置（不静默）", () => {
    expect(isQuietNow({ quietEnabled: true, quietStart: 3, quietEnd: 3 }, at(3))).toBe(false);
  });
});
