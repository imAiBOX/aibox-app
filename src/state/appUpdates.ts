/**
 * 更新提醒纯逻辑（可单测，时间/数据全注入）：
 * - diffAppUpdates：已装 vs 上游 latest 比对（mock 同构 + 单测；真分支在 Rust check_app_updates）
 * - diffNewApps：目录新增 diff（首启无记录 → 不提示，避免首启轰炸）
 * - isCheckDue：检查时间门控（默认 4h，防频繁拉）
 */
import type { AppUpdateRow } from "../net/client";

export interface InstalledApp {
  id: string;
  version?: string;
}

/** 结果缓存门控时长：4h（kv 时间戳） */
export const APP_UPDATE_TTL_MS = 4 * 60 * 60 * 1000;

/** 版本比较（v 前缀可选；数字段逐段比，非数字段字典序）。candidate > current。 */
export function isNewer(candidate: string, current: string): boolean {
  const segs = (s: string): string[] =>
    s
      .trim()
      .replace(/^v/, "")
      .split(/[.\-+]/);
  const a = segs(candidate);
  const b = segs(current);
  const n = Math.max(a.length, b.length);
  for (let i = 0; i < n; i++) {
    const x = a[i] ?? "0";
    const y = b[i] ?? "0";
    const xn = /^\d+$/.test(x);
    const yn = /^\d+$/.test(y);
    if (xn && yn) {
      const xv = Number(x);
      const yv = Number(y);
      if (xv !== yv) return xv > yv;
    } else if (x !== y) {
      return x > y;
    }
  }
  return false;
}

/**
 * 已装 vs 上游 latest → 更新行（形状 = Rust check_app_updates 返回）。
 * 缺 latest（registry 无记录/拉取失败）或缺 installed → 不提醒（缺数据不打扰）。
 */
export function diffAppUpdates(
  installed: InstalledApp[],
  latestById: Record<string, string>,
): AppUpdateRow[] {
  return installed.map((a) => {
    const latest = latestById[a.id];
    const hasUpdate = Boolean(latest && a.version && isNewer(latest, a.version));
    return {
      appId: a.id,
      ...(a.version ? { installed: a.version } : {}),
      ...(latest ? { latest } : {}),
      hasUpdate,
    };
  });
}

/**
 * 目录新增 diff：seen 为 null（首启/无记录）→ []（不提示，仅由调用方落记录）；
 * 否则返回 current 中没见过的 id（保序、去重）。
 */
export function diffNewApps(seen: string[] | null, current: string[]): string[] {
  if (seen === null) return [];
  const known = new Set(seen);
  const out: string[] = [];
  for (const id of current) {
    if (!known.has(id) && !out.includes(id)) out.push(id);
  }
  return out;
}

/** 检查时间门控：无记录 → 到期；距上次检查 ≥ ttl → 到期。now 可注入。 */
export function isCheckDue(
  lastAt: number | null | undefined,
  now: number,
  ttlMs: number,
): boolean {
  if (lastAt === null || lastAt === undefined || !Number.isFinite(lastAt)) return true;
  return now - lastAt >= ttlMs;
}
