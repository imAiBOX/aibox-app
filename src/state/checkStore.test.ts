/**
 * 更新检查 kv 缓存：注入 fake Kv 往返 + 后端选择降级（无 localStorage → 内存）。
 */
import { describe, expect, it } from "vitest";
import { createCheckStore } from "./checkStore";
import type { Kv } from "./persistence-sqlite";

function fakeKv(): Kv & { data: Map<string, string> } {
  const data = new Map<string, string>();
  return {
    data,
    async get<T>(key: string) {
      const raw = data.get(key);
      return raw === undefined ? null : (JSON.parse(raw) as T);
    },
    async set(key: string, value: unknown) {
      data.set(key, JSON.stringify(value));
    },
  };
}

describe("createCheckStore（kv 后端）", () => {
  it("4h 缓存行与 seen id 往返；无记录 → null", async () => {
    const kv = fakeKv();
    const store = createCheckStore(kv);
    expect(await store.loadAppUpdates()).toBeNull();
    expect(await store.loadSeenAppIds()).toBeNull();

    await store.saveAppUpdates({
      checkedAt: 12345,
      rows: [{ appId: "a", installed: "1.0.0", latest: "1.1.0", hasUpdate: true }],
    });
    const c = await store.loadAppUpdates();
    expect(c?.checkedAt).toBe(12345);
    expect(c?.rows[0].hasUpdate).toBe(true);

    await store.saveSeenAppIds(["a", "b"]);
    expect(await store.loadSeenAppIds()).toEqual(["a", "b"]);
  });

  it("写入覆盖旧值（时间戳单调推进）", async () => {
    const kv = fakeKv();
    const store = createCheckStore(kv);
    await store.saveAppUpdates({ checkedAt: 1, rows: [] });
    await store.saveAppUpdates({ checkedAt: 2, rows: [] });
    expect((await store.loadAppUpdates())?.checkedAt).toBe(2);
  });
});

describe("createCheckStore（mem 后端降级）", () => {
  it("注入 null → 内存实现可用（浏览器/单测无 localStorage 也不炸）", async () => {
    const store = createCheckStore(null);
    expect(await store.loadSeenAppIds()).toBeNull();
    await store.saveSeenAppIds(["x"]);
    expect(await store.loadSeenAppIds()).toEqual(["x"]);
  });
});
