/**
 * 通知偏好（用户说了算；持久化）。
 *
 * **为什么要有**：AiBOX 常驻后如果什么都通知，用户几天内就会把通知全关掉 ——
 * 那样「有升级」这个价值就废了。所以要做到**可关、可调、且默认克制**：
 * - 首次运行/装完 3 天内不上新提醒（用户还没熟悉，先别吵）
 * - 「上新应用」每天最多 1 条
 * - 同一事件不重复通知（已通知过的版本/应用 id 记下来）
 *
 * 存储：真 Tauri → SQLite kv；mock/浏览器 → localStorage（不可用则内存兜底）。
 * 任何存储错误静默降级 —— 偏好读不到就用默认值，绝不阻塞。
 */
import { isMock } from "../net/client";
import { createKv, isTauriEnv, tauriSqlDriver, type Kv } from "./persistence-sqlite";

export interface NotifyPrefs {
  /** 本体有新版本 → 通知 */
  coreUpdate: boolean;
  /** 已装应用有升级 → 通知 */
  appUpdate: boolean;
  /** 目录上新应用 → 通知 */
  newApps: boolean;
  /** 启动后自动收进托盘（不弹窗口） */
  startMinimized: boolean;
  /** 装完一个应用后自动收进托盘 */
  minimizeAfterInstall: boolean;
  /** 免打扰时段开启（夜里不弹通知，醒来再提醒） */
  quietEnabled: boolean;
  /** 免打扰开始（0-23 小时） */
  quietStart: number;
  /** 免打扰结束（0-23 小时） */
  quietEnd: number;
}

/**
 * 当前是否处于免打扰时段（纯函数）。
 *
 * 支持跨零点（22:00 → 08:00）：start > end 时视为「跨天」。
 */
export function isQuietNow(
  prefs: Pick<NotifyPrefs, "quietEnabled" | "quietStart" | "quietEnd">,
  now: Date = new Date(),
): boolean {
  if (!prefs.quietEnabled) return false;
  const h = now.getHours();
  const { quietStart: s, quietEnd: e } = prefs;
  if (s === e) return false;
  return s < e ? h >= s && h < e : h >= s || h < e;
}

export const DEFAULT_NOTIFY_PREFS: NotifyPrefs = {
  coreUpdate: true,
  appUpdate: true,
  newApps: true,
  startMinimized: false,
  minimizeAfterInstall: true,
  quietEnabled: false,
  quietStart: 22,
  quietEnd: 8,
};

const KV_KEY = "notify.prefs";
const LS_KEY = "aibox.notify.prefs";

/** 已通知过的标记（避免同一事件重复打扰） */
const KV_SEEN = "notify.seen";
const LS_SEEN = "aibox.notify.seen";

/** 最近一次「升级前备份」的记录（用户可自行恢复） */
export interface UpgradeBackupRecord {
  appId: string;
  path: string;
  at: number;
}

export interface NotifyStore {
  loadPrefs(): Promise<NotifyPrefs>;
  savePrefs(p: NotifyPrefs): Promise<void>;
  /** 取已通知集合 */
  loadSeen(): Promise<string[]>;
  /** 记入已通知（返回是否首次） */
  markSeen(key: string): Promise<boolean>;
  /** 记下最近一次升级前备份 */
  saveUpgradeBackup(r: UpgradeBackupRecord): Promise<void>;
  /** 读最近一次升级前备份（无 → null） */
  loadUpgradeBackup(): Promise<UpgradeBackupRecord | null>;
}

export function createNotifyStore(kv?: Kv | null): NotifyStore {
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
      /* 隐私模式 → 内存 */
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
      /* fallthrough */
    }
    mem.set(k, v);
  };

  return {
    async loadPrefs() {
      try {
        const raw =
          backend === "kv"
            ? await resolveKv()?.get<Partial<NotifyPrefs>>(KV_KEY)
            : lsGet(LS_KEY)
              ? (JSON.parse(lsGet(LS_KEY) as string) as Partial<NotifyPrefs>)
              : null;
        if (raw) return { ...DEFAULT_NOTIFY_PREFS, ...raw };
      } catch {
        /* 损坏 → 默认值 */
      }
      return { ...DEFAULT_NOTIFY_PREFS };
    },
    async savePrefs(p) {
      try {
        if (backend === "kv") {
          const k = resolveKv();
          if (k) {
            await k.set(KV_KEY, p);
            return;
          }
        }
        lsSet(LS_KEY, JSON.stringify(p));
      } catch {
        /* 尽力而为 */
      }
    },
    async loadSeen() {
      try {
        const raw =
          backend === "kv"
            ? await resolveKv()?.get<string[]>(KV_SEEN)
            : lsGet(LS_SEEN)
              ? (JSON.parse(lsGet(LS_SEEN) as string) as string[])
              : null;
        return Array.isArray(raw) ? raw : [];
      } catch {
        return [];
      }
    },
    async saveUpgradeBackup(r) {
      try {
        if (backend === "kv") {
          const k = resolveKv();
          if (k) {
            await k.set("notify.upgradeBackup", r);
            return;
          }
        }
        lsSet("aibox.upgradeBackup", JSON.stringify(r));
      } catch {
        /* 尽力而为 */
      }
    },
    async loadUpgradeBackup() {
      try {
        const raw =
          backend === "kv"
            ? await resolveKv()?.get<UpgradeBackupRecord>("notify.upgradeBackup")
            : lsGet("aibox.upgradeBackup")
              ? (JSON.parse(lsGet("aibox.upgradeBackup") as string) as UpgradeBackupRecord)
              : null;
        return raw && typeof raw.path === "string" ? raw : null;
      } catch {
        return null;
      }
    },
    async markSeen(key) {
      try {
        const seen = await this.loadSeen();
        if (seen.includes(key)) return false;
        const next = [...seen, key].slice(-200);
        if (backend === "kv") {
          const k = resolveKv();
          if (k) {
            await k.set(KV_SEEN, next);
            return true;
          }
        }
        lsSet(LS_SEEN, JSON.stringify(next));
        return true;
      } catch {
        return true;
      }
    },
  };
}
