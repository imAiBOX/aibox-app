/**
 * S1 卡片计数（views/installs）展示纯逻辑（可单测，数据全注入）：
 * - formatCount：紧凑数字格式化（1234 → "1.2k"，1_250_000 → "1.2M"）
 * - appCounts：按 id 取计数；无数据 / 全零 → null（不显示，避免零噪音）
 */
import type { StatsPayload } from "../net/client";

/** 千分位截断为一位小数，去掉多余的 ".0"（1.0k → 1k） */
function trim1(x: number): string {
  const s = (Math.round(x * 10) / 10).toString();
  return s.endsWith(".0") ? s.slice(0, -2) : s;
}

/** 数字 → 紧凑展示串。非法/负数 → "0"。 */
export function formatCount(n: number): string {
  if (!Number.isFinite(n) || n < 0) return "0";
  const v = Math.round(n);
  if (v < 1000) return String(v);
  if (v < 1_000_000) return trim1(v / 1000) + "k";
  return trim1(v / 1_000_000) + "M";
}

/** 取某应用计数；缺条目或 views/installs 全 ≤0 → null（卡片不渲染该行）。 */
export function appCounts(
  stats: StatsPayload | null | undefined,
  id: string,
): { views: number; installs: number } | null {
  const c = stats?.apps?.[id];
  if (!c) return null;
  const views = Number.isFinite(c.views) ? Math.max(0, Math.round(c.views)) : 0;
  const installs = Number.isFinite(c.installs) ? Math.max(0, Math.round(c.installs)) : 0;
  if (views <= 0 && installs <= 0) return null;
  return { views, installs };
}
