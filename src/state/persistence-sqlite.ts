/**
 * 断点续装 SQLite 实现。
 *
 * - 真 Tauri 环境：tauri-plugin-sql（sqlite），表 `sessions` + `kv`，建表幂等。
 * - mock / 浏览器（无 __TAURI_INTERNALS__）：回落 memory（persistence.ts 既有实现）。
 * - 任何 SQLite 错误静默降级到内存副本——持久化绝不阻塞安装流程。
 *
 * 不改 Persistence 接口定义；「继续上次安装？」流程不变，仅换实现。
 * 单测：注入 fake SqlDriver 断言 SQL 构造（离线，不依赖真库）。
 */
import { isMock } from "../net/client";
import { memoryPersistence, type Persistence } from "./persistence";
import type { SessionSnapshot } from "./types";

/* ── driver 抽象（可注入 fake 测试） ── */
export interface SqlDriver {
  execute(query: string, values?: unknown[]): Promise<void>;
  select<T = Record<string, unknown>>(query: string, values?: unknown[]): Promise<T[]>;
}

/** 建表语句（幂等；冻结的 schema） */
export const SCHEMA: readonly string[] = [
  "CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, state_json TEXT NOT NULL, updated_at INTEGER NOT NULL)",
  "CREATE TABLE IF NOT EXISTS kv (key TEXT PRIMARY KEY, value_json TEXT NOT NULL)",
];

const SESSION_ID = "current";
const DB_NAME = "sqlite:aibox.db";

/** Tauri v2 运行时探测（浏览器/单测 → false） */
export function isTauriEnv(): boolean {
  return typeof globalThis !== "undefined" && "__TAURI_INTERNALS__" in globalThis;
}

/**
 * 真 Tauri driver：直接调 tauri-plugin-sql 的插件 command
 * （load→幂等建库；execute/select 来自 capability sql:default + sql:allow-execute）。
 */
export function tauriSqlDriver(dbName: string = DB_NAME): SqlDriver {
  let ready: Promise<void> | null = null;
  const ensure = (): Promise<void> =>
    (ready ??= (async () => {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("plugin:sql|load", { db: dbName });
    })());
  return {
    async execute(query, values = []) {
      await ensure();
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("plugin:sql|execute", { db: dbName, query, values });
    },
    async select<T = Record<string, unknown>>(query: string, values: unknown[] = []) {
      await ensure();
      const { invoke } = await import("@tauri-apps/api/core");
      return (await invoke<Array<Record<string, unknown>>>("plugin:sql|select", {
        db: dbName,
        query,
        values,
      })) as unknown as T[];
    },
  };
}

/** SQLite 版 Persistence（注入 driver；schema 幂等初始化） */
export function sqlitePersistence(driver: SqlDriver): Persistence {
  let ready: Promise<void> | null = null;
  const init = (): Promise<void> =>
    (ready ??= (async () => {
      for (const sql of SCHEMA) await driver.execute(sql);
    })());
  return {
    async loadSession() {
      await init();
      const rows = await driver.select<{ state_json: string }>(
        "SELECT state_json FROM sessions WHERE id = ?",
        [SESSION_ID],
      );
      if (rows.length === 0) return null;
      try {
        return JSON.parse(rows[0].state_json) as SessionSnapshot;
      } catch {
        return null; // 损坏快照 → 视为无会话（不阻塞）
      }
    },
    async saveSnapshot(snap) {
      await init();
      await driver.execute(
        "INSERT INTO sessions (id, state_json, updated_at) VALUES (?, ?, ?) " +
          "ON CONFLICT(id) DO UPDATE SET state_json = excluded.state_json, updated_at = excluded.updated_at",
        [SESSION_ID, JSON.stringify(snap), Date.now()],
      );
    },
    async clearSession() {
      await init();
      await driver.execute("DELETE FROM sessions WHERE id = ?", [SESSION_ID]);
    },
  };
}

/** SQLite 错误静默降级到内存副本（持久化永不阻塞主流程） */
function withFallback(sql: Persistence): Persistence {
  const mem = memoryPersistence();
  return {
    async loadSession() {
      try {
        return await sql.loadSession();
      } catch {
        return mem.loadSession();
      }
    },
    async saveSnapshot(snap) {
      try {
        await sql.saveSnapshot(snap);
      } catch {
        await mem.saveSnapshot(snap);
      }
    },
    async clearSession() {
      try {
        await sql.clearSession();
      } catch {
        await mem.clearSession();
      }
    },
  };
}

/**
 * persistence 工厂（流程不变，仅换实现）：
 * - 注入 driver → SQLite（测试）
 * - mock 或非 Tauri（浏览器/CI）→ memory
 * - 真 Tauri → SQLite（错误降级 memory）
 */
export function createPersistence(
  opts: { mock?: boolean; tauri?: boolean; driver?: SqlDriver } = {},
): Persistence {
  if (opts.driver) return withFallback(sqlitePersistence(opts.driver));
  const mock = opts.mock ?? isMock();
  const tauri = opts.tauri ?? isTauriEnv();
  if (mock || !tauri) return memoryPersistence();
  return withFallback(sqlitePersistence(tauriSqlDriver()));
}

/* ── kv（settings 等键值；同一 db） ── */
export interface Kv {
  get<T = unknown>(key: string): Promise<T | null>;
  set(key: string, value: unknown): Promise<void>;
}

export function createKv(driver: SqlDriver): Kv {
  let ready: Promise<void> | null = null;
  const init = (): Promise<void> =>
    (ready ??= (async () => {
      for (const sql of SCHEMA) await driver.execute(sql);
    })());
  return {
    async get<T>(key: string): Promise<T | null> {
      await init();
      const rows = await driver.select<{ value_json: string }>(
        "SELECT value_json FROM kv WHERE key = ?",
        [key],
      );
      if (rows.length === 0) return null;
      try {
        return JSON.parse(rows[0].value_json) as T;
      } catch {
        return null;
      }
    },
    async set(key: string, value: unknown) {
      await init();
      await driver.execute(
        "INSERT INTO kv (key, value_json) VALUES (?, ?) " +
          "ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json",
        [key, JSON.stringify(value)],
      );
    },
  };
}
