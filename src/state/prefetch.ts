/**
 * 空闲预下载纯逻辑（可测）：
 * - `prefetchQueue`：一轮 check_app_updates 结果 → 待预下载 appId 队列（去重、保序）
 * - `prefetchGate`：只在常驻空闲（IDLE）触发，安装/诊断等进行中不抢带宽
 */
import type { AppUpdateRow } from "../net/client";

/** 有新版的 appId（去重、保持检查结果顺序）；每轮新结果整体重建 */
export function prefetchQueue(rows: AppUpdateRow[]): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const r of rows) {
    if (r.hasUpdate && r.appId && !seen.has(r.appId)) {
      seen.add(r.appId);
      out.push(r.appId);
    }
  }
  return out;
}

/** 触发门控：仅 IDLE（常驻空闲）；其余状态暂存，回到 IDLE 再继续 */
export function prefetchGate(status: string): boolean {
  return status === "IDLE";
}
