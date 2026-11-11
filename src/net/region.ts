/**
 * 区域判定的前端兜底（`net_region()` invoke 失败时的静态判定）。
 *
 * 纯函数（语言/时区可注入，离线单测）：
 * - `navigator.language` 以 `zh` 开头，或时区为 `Asia/Shanghai` → `cn`
 * - 否则 → `global`
 *
 * 兜底只影响路线选择（宁可保守），真实判定仍以 Rust 三锚点实测为准；
 * 失败时 console.warn 留痕（诊断可见）。
 */

export type RegionKind = "cn" | "global";

/** 纯判定：给定语言与时区 → 区域初值（可注入，单测离线）。 */
export function staticRegionOf(language: string, timeZone: string): RegionKind {
  const zh = language.trim().toLowerCase().startsWith("zh");
  const shanghai = timeZone.trim() === "Asia/Shanghai";
  return zh || shanghai ? "cn" : "global";
}

/** 浏览器环境静态判定（读 navigator.language / Intl 时区）。 */
export function staticRegionFallback(): RegionKind {
  try {
    const lang =
      (typeof navigator !== "undefined" && (navigator.language || "")) || "";
    const tz =
      (typeof Intl !== "undefined" && Intl.DateTimeFormat().resolvedOptions().timeZone) ||
      "";
    return staticRegionOf(lang, tz);
  } catch {
    return "global";
  }
}
