/**
 * 网络页本地设置存储（通用 KV 工具）。
 * 真 Tauri → SQLite kv 表；mock/浏览器 → localStorage（不可用则内存兜底）。
 *
 * **注意**：原本的「PROXY 开关 / CUSTOM 前缀」字段已下线 —— 它们只写不读，
 * 下载层（channels 来自 registry）根本不消费，开关形同虚设，却把用户引向无效操作。
 * 地区选择不走这里——它经 `net_region_set` 写 Rust 侧 settings.json（真源）。
 */
import { isMock } from "../net/client";
import { createKv, isTauriEnv, tauriSqlDriver, type Kv } from "./persistence-sqlite";

/** 网络页本地设置（通用键值对；具体字段由使用方定义） */
export type NetLocalSettings = Record<string, unknown>;

export const DEFAULT_NET_SETTINGS: NetLocalSettings = {};

const LS_KEY = "aibox.net.settings";
const KV_KEY = "net.settings";

/** 可注入后端（测试用）；缺省按环境选择 */
export function netSettingsStore(kv?: Kv | null): {
  load(): Promise<NetLocalSettings>;
  save(s: NetLocalSettings): Promise<void>;
} {
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

  return {
    async load() {
      try {
        if (backend === "kv") {
          const k = resolveKv();
          const raw = k ? await k.get<Partial<NetLocalSettings>>(KV_KEY) : null;
          if (raw) return { ...DEFAULT_NET_SETTINGS, ...raw };
        } else {
          const raw = lsGet(LS_KEY);
          if (raw) return { ...DEFAULT_NET_SETTINGS, ...(JSON.parse(raw) as Partial<NetLocalSettings>) };
        }
      } catch {
        /* 损坏/不可用 → 默认值 */
      }
      return { ...DEFAULT_NET_SETTINGS };
    },
    async save(s) {
      try {
        if (backend === "kv") {
          const k = resolveKv();
          if (k) {
            await k.set(KV_KEY, s);
            return;
          }
        }
        lsSet(LS_KEY, JSON.stringify(s));
      } catch {
        /* 尽力而为：设置丢失不阻塞 */
      }
    },
  };
}
