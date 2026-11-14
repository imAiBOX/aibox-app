/**
 * 空闲预下载编排：
 * 一轮 check_app_updates 结果里有新版 → 常驻空闲（IDLE）时顺序静默预下载主安装包。
 * - 每轮新检查结果重建队列；非 IDLE 时暂存、回到 IDLE 继续（不丢轮次）
 * - 全程 fire-and-forget：开关/忙/失败由 Rust 判定，前端零 UI、零阻塞
 * - 关（settings prefetch.enabled=false）→ Rust 返回 disabled，前端不再连发
 */
import { useEffect, useRef } from "react";
import { api, type AppUpdateRow } from "../net/client";
import { prefetchGate, prefetchQueue } from "../state/prefetch";

export function usePrefetch(rows: AppUpdateRow[], status: string): void {
  const sourceRef = useRef<AppUpdateRow[] | null>(null);
  const pendingRef = useRef<string[]>([]);
  // 渲染期同步换轮：新一轮检查结果 → 重建待预下载队列
  if (sourceRef.current !== rows) {
    sourceRef.current = rows;
    pendingRef.current = prefetchQueue(rows);
  }
  const pending = pendingRef.current;
  const gate = prefetchGate(status);

  useEffect(() => {
    if (!gate || pending.length === 0) return;
    const queue = pendingRef.current;
    pendingRef.current = [];
    void (async () => {
      try {
        const st = await api.prefetchGet();
        if (!st.enabled) return; // 开关关了 → 不触发新预下载（已有缓存保留）
        for (const appId of queue) {
          try {
            await api.prefetchStart(appId); // 顺序单飞；结果只进任务日志
          } catch {
            /* busy/failed 静默：下轮 4h 再试 */
          }
        }
      } catch {
        /* 设置读取失败静默（best-effort，不打扰） */
      }
    })();
  }, [gate, pending, rows, status]);
}
