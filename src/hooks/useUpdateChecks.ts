/**
 * 应用更新/目录新增检查编排：
 * - 启动时一次 + 常驻每 4h（窗口隐藏也跑）；结果 4h kv 时间戳门控
 * - 托盘「检查更新」→ recheck({force:true}) 绕过 4h 门控（registry 12h / 本体 5min 既有缓存仍生效）
 * - 目录新增：首启（无 seen 记录）只记录不提示
 * 纯逻辑（diff / 门控）在 state/appUpdates.ts；这里只做编排。
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type AppUpdateRow } from "../net/client";
import { APP_UPDATE_TTL_MS, diffNewApps, isCheckDue } from "../state/appUpdates";
import { createCheckStore, type CheckStore } from "../state/checkStore";

export interface UpdateChecks {
  appUpdates: AppUpdateRow[];
  newApps: string[];
  dismissNewApps: () => void;
  /** 目录新增比对（registry_index 拉取后调用；首启不提示） */
  checkNewApps: (currentIds: string[]) => Promise<void>;
  /** 三类检查之一：已装应用更新（4h 门控；force = 手动检查） */
  recheck: (opts?: { force?: boolean }) => Promise<void>;
}

export function useUpdateChecks(): UpdateChecks {
  const storeRef = useRef<CheckStore | null>(null);
  storeRef.current ??= createCheckStore();
  const store = storeRef.current;
  const [appUpdates, setAppUpdates] = useState<AppUpdateRow[]>([]);
  const [newApps, setNewApps] = useState<string[]>([]);

  const recheck = useCallback(
    async (opts?: { force?: boolean }) => {
      try {
        const force = opts?.force === true;
        const now = Date.now();
        let rows: AppUpdateRow[];
        if (!force) {
          const cached = await store.loadAppUpdates();
          if (isCheckDue(cached?.checkedAt ?? null, now, APP_UPDATE_TTL_MS)) {
            rows = await api.checkAppUpdates();
            await store.saveAppUpdates({ checkedAt: now, rows });
          } else {
            rows = cached?.rows ?? [];
          }
        } else {
          rows = await api.checkAppUpdates();
          await store.saveAppUpdates({ checkedAt: now, rows });
        }
        setAppUpdates(rows);
      } catch {
        /* 检查失败静默（克制不打扰；下个周期重试） */
      }
    },
    [store],
  );

  const checkNewApps = useCallback(
    async (currentIds: string[]) => {
      try {
        const seen = await store.loadSeenAppIds();
        if (seen === null) {
          // 首启（无记录）→ 只记录，不提示（避免首启轰炸）
          await store.saveSeenAppIds(currentIds);
          return;
        }
        const added = diffNewApps(seen, currentIds);
        if (added.length > 0) {
          setNewApps(added);
          await store.saveSeenAppIds([...new Set([...seen, ...currentIds])]);
        }
      } catch {
        /* 存储不可用 → 不提示 */
      }
    },
    [store],
  );

  const dismissNewApps = useCallback(() => setNewApps([]), []);

  // 启动一次 + 每 4h（webview 隐藏时定时器照常运行）
  useEffect(() => {
    void recheck();
    const id = setInterval(() => void recheck(), APP_UPDATE_TTL_MS);
    return () => clearInterval(id);
  }, [recheck]);

  return { appUpdates, newApps, dismissNewApps, checkNewApps, recheck };
}
