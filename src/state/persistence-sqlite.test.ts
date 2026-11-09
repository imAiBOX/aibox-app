/**
 * persistence-sqlite 测试：注入 fake driver 断言 SQL 构造（离线，不依赖真库）
 * + createPersistence 工厂回落逻辑（mock/浏览器 → memory；注入 driver → SQLite）。
 */
import { describe, expect, it } from "vitest";
import {
  createKv,
  createPersistence,
  SCHEMA,
  sqlitePersistence,
  type SqlDriver,
} from "./persistence-sqlite";
import { memoryPersistence } from "./persistence";
import type { SessionSnapshot } from "./types";

interface Call {
  kind: "execute" | "select";
  query: string;
  values: unknown[];
}

function fakeDriver(rows: Record<string, unknown>[] = []): SqlDriver & { calls: Call[] } {
  const calls: Call[] = [];
  return {
    calls,
    async execute(query: string, values: unknown[] = []) {
      calls.push({ kind: "execute", query, values });
    },
    async select<T>(query: string, values: unknown[] = []) {
      calls.push({ kind: "select", query, values });
      return rows as T[];
    },
  };
}

const snap: SessionSnapshot = { appId: "openclaw", doneTaskIds: ["node", "git"], savedAt: 42 };

describe("sqlitePersistence（fake driver）", () => {
  it("初始化执行幂等建表（CREATE TABLE IF NOT EXISTS sessions + kv）", async () => {
    const d = fakeDriver();
    await sqlitePersistence(d).loadSession();
    const execs = d.calls.filter((c) => c.kind === "execute").map((c) => c.query);
    expect(execs[0]).toContain("CREATE TABLE IF NOT EXISTS sessions");
    expect(execs[0]).toContain("state_json");
    expect(execs[1]).toContain("CREATE TABLE IF NOT EXISTS kv");
    // 幂等关键词齐全
    for (const q of execs) expect(q).toContain("IF NOT EXISTS");
    expect(SCHEMA).toHaveLength(2);
  });

  it("saveSnapshot → UPSERT 单行（id=current，JSON 快照含任务队列边界）", async () => {
    const d = fakeDriver();
    await sqlitePersistence(d).saveSnapshot(snap);
    const upsert = d.calls.find((c) => c.kind === "execute" && c.query.includes("INSERT INTO sessions"));
    expect(upsert).toBeTruthy();
    expect(upsert!.query).toContain("ON CONFLICT(id) DO UPDATE");
    expect(upsert!.values[0]).toBe("current");
    const parsed = JSON.parse(String(upsert!.values[1])) as SessionSnapshot;
    expect(parsed).toEqual(snap);
    expect(typeof upsert!.values[2]).toBe("number");
  });

  it("loadSession 解析行 / 空表 → null / 损坏 JSON → null（不抛）", async () => {
    const ok = fakeDriver([{ state_json: JSON.stringify(snap) }]);
    expect(await sqlitePersistence(ok).loadSession()).toEqual(snap);

    const empty = fakeDriver([]);
    expect(await sqlitePersistence(empty).loadSession()).toBeNull();

    const broken = fakeDriver([{ state_json: "{oops" }]);
    expect(await sqlitePersistence(broken).loadSession()).toBeNull();
  });

  it("clearSession → DELETE 单行", async () => {
    const d = fakeDriver();
    await sqlitePersistence(d).clearSession();
    const del = d.calls.find((c) => c.query.startsWith("DELETE FROM sessions"));
    expect(del).toBeTruthy();
    expect(del!.values).toEqual(["current"]);
  });

  it("多次调用只初始化一次 schema（幂等 + 缓存）", async () => {
    const d = fakeDriver();
    const p = sqlitePersistence(d);
    await p.loadSession();
    await p.saveSnapshot(snap);
    const creates = d.calls.filter((c) => c.query.includes("CREATE TABLE"));
    expect(creates).toHaveLength(2); // 仅首轮建表
  });
});

describe("createPersistence 工厂回落", () => {
  it("mock=true → memory（store 可见）", async () => {
    const p = createPersistence({ mock: true });
    await p.saveSnapshot(snap);
    // memory 实现有 store 字段；SQLite 分支没有 → 以此断言回落
    expect((p as { store?: unknown }).store).toBeTruthy();
    expect(await p.loadSession()).toEqual(snap);
  });

  it("非 Tauri 浏览器（tauri=false）→ memory", async () => {
    const p = createPersistence({ mock: false, tauri: false });
    expect((p as { store?: unknown }).store).toBeTruthy();
  });

  it("注入 driver → SQLite 分支（无 store，SQL 可断言）", async () => {
    const d = fakeDriver();
    const p = createPersistence({ driver: d });
    expect((p as { store?: unknown }).store).toBeUndefined();
    await p.saveSnapshot(snap);
    expect(d.calls.some((c) => c.query.includes("INSERT INTO sessions"))).toBe(true);
  });

  it("SQLite 抛错 → 静默降级 memory（save/load 不抛）", async () => {
    const boom: SqlDriver = {
      async execute() {
        throw new Error("db locked");
      },
      async select() {
        throw new Error("db locked");
      },
    };
    const p = createPersistence({ driver: boom });
    await expect(p.saveSnapshot(snap)).resolves.toBeUndefined();
    // 降级后读到的是内存副本里的值（首轮 load 已吞错）
    await expect(p.loadSession()).resolves.toEqual(snap);
  });

  it("memoryPersistence 接口形状不变", () => {
    const p = memoryPersistence();
    expect(typeof p.loadSession).toBe("function");
    expect(typeof p.saveSnapshot).toBe("function");
    expect(typeof p.clearSession).toBe("function");
  });
});

describe("createKv（settings 持久化）", () => {
  it("set → INSERT INTO kv UPSERT；get → SELECT + 解析", async () => {
    const d = fakeDriver();
    const kv = createKv(d);
    await kv.set("net.settings", { proxyOn: true });
    const up = d.calls.find((c) => c.query.includes("INSERT INTO kv"));
    expect(up).toBeTruthy();
    expect(up!.values).toEqual(["net.settings", JSON.stringify({ proxyOn: true })]);

    const kv2 = createKv(fakeDriver([{ value_json: '{"proxyOn":true}' }]));
    expect(await kv2.get("net.settings")).toEqual({ proxyOn: true });
    const kv3 = createKv(fakeDriver([]));
    expect(await kv3.get("missing")).toBeNull();
  });
});
