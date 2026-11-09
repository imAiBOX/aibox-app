/**
 * persistence 接口测试（memory 实现；换 SQLite 实现时接口不变）。
 */
import { describe, expect, it } from "vitest";
import { doneIdsOf, memoryPersistence } from "./persistence";
import type { SessionSnapshot } from "./types";

const snap: SessionSnapshot = {
  appId: "openclaw",
  doneTaskIds: ["node", "git"],
  savedAt: 123,
};

describe("memoryPersistence", () => {
  it("初始为空", async () => {
    const p = memoryPersistence();
    expect(await p.loadSession()).toBeNull();
  });

  it("save → load 往返", async () => {
    const p = memoryPersistence();
    await p.saveSnapshot(snap);
    expect(await p.loadSession()).toEqual(snap);
  });

  it("clearSession 清空", async () => {
    const p = memoryPersistence(snap);
    await p.clearSession();
    expect(await p.loadSession()).toBeNull();
  });

  it("可注入初始快照（模拟已存在会话）", async () => {
    const p = memoryPersistence(snap);
    expect(doneIdsOf(await p.loadSession())).toEqual(["node", "git"]);
  });

  it("doneIdsOf(null) → []", () => {
    expect(doneIdsOf(null)).toEqual([]);
  });
});
