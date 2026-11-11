/**
 * 更新检查 kv 缓存（4h 时间戳 + 结果行 + 见过的 app id 集合）。
 * 真 Tauri → SQLite kv 表（与断点续装同库）；mock/浏览器 → localStorage（不可用则内存兜底）。
 * 任何存储错误静默降级——检查缓存绝不阻塞 UI。
 */
import type { AppUpdateRow } from "../net/client";
import { isMock } from "../net/client";
import { createKv, isTauriEnv, tauriSqlDriver, type Kv } from "./persistence-sqlite";

export interface AppUpdateCache {
  /** 上次成功检查的时间戳（ms）；4h 门控用 */
  checkedAt: number;
  rows: AppUpdateRow[];
}

export interface CheckStore {
  loadAppUpdates(): Promise<AppUpdateCache | null>;
  saveAppUpdates(c: AppUpdateCache): Promise<void>;
  loadSeenAppIds(): Promise<string[] | null>;
  saveSeenAppIds(ids: string[]): Promise<void>;
}

const KV_CACHE = "appUpdates.cache";
const KV_SEEN = "appUpdates.seenIds";
const LS_PREFIX = "aibox.checks.";

/** 可注入后端（测试用）；缺省按环境选择：Tauri → kv，否则 localStorage/内存 */
export function createCheckStore(kv?: Kv | null): CheckStore {
  const backend: "kv" | "ls" | "mem" =
    kv !== undefined
      ? kv
        ? "kv"
        : "mem"
      : !isMock() && isTauriEnv()
        ? "kv"
        : "ls";
  const mem = new Map<string, string>();
  const resolveKv = (): Kv | null => {
    if (kv !== undefined) return kv;
    if (backend === "kv") {
      try {
        return createKv(tauriSqlDriver());
      } catch {
        return null;
      }
    }
    return null;
  };
  const lsGet = (k: string): string | null => {
    try {
      if (typeof localStorage !== "undefined") return localStorage.getItem(k);
    } catch {
      /* 隐私模式等 → 走内存 */
    }
    return mem.get(k) ?? null;
  };
  const lsSet = (k: string, v: string): void => {
    try {
      if (typeof localStorage !== "undefined") {
        localStorage.setItem(k, v);
        return;
      }
    } catch {
      /* fallthrough → 内存 */
    }
    mem.set(k, v);
  };

  const load = async <T>(kvKey: string): Promise<T | null> => {
    try {
      if (backend === "kv") {
        const k = resolveKv();
        if (k) return await k.get<T>(kvKey);
      }
      const raw = lsGet(LS_PREFIX + kvKey);
      return raw ? (JSON.parse(raw) as T) : null;
    } catch {
      return null;
    }
  };
  const save = async (kvKey: string, value: unknown): Promise<void> => {
    try {
      if (backend === "kv") {
        const k = resolveKv();
        if (k) {
          await k.set(kvKey, value);
          return;
        }
      }
      lsSet(LS_PREFIX + kvKey, JSON.stringify(value));
    } catch {
      /* 尽力而为：缓存丢失不阻塞（下次重查） */
    }
  };

  return {
    loadAppUpdates: () => load<AppUpdateCache>(KV_CACHE),
    saveAppUpdates: (c) => save(KV_CACHE, c),
    loadSeenAppIds: () => load<string[]>(KV_SEEN),
    saveSeenAppIds: (ids) => save(KV_SEEN, ids),
  };
}
