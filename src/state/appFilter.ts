/**
 * S1 应用目录筛选纯逻辑（可单测，无 IO）：
 * - categoryOf / deriveCategories：分类从数据推导（不硬编码枚举），缺省归 "other"
 * - filterApps：组合过滤（先分类后搜索），name + tagline 全语言值，大小写不敏感
 */
import type { AppIndexEntry } from "../net/client";

/** 「全部」分类的哨兵值（chips 默认项） */
export const ALL_CATEGORIES = "all";

export interface CategoryCount {
  id: string;
  count: number;
}

/** 单应用的分类 id：空白/缺失 → "other"（保证任何卡片都能被某个 chip 覆盖） */
export function categoryOf(a: AppIndexEntry): string {
  const c = a.category?.trim();
  return c ? c : "other";
}

/** 分类 chips 数据源：按首次出现顺序去重 + 计数（未知 id 原样保留） */
export function deriveCategories(apps: AppIndexEntry[]): CategoryCount[] {
  const map = new Map<string, number>();
  for (const a of apps) {
    const id = categoryOf(a);
    map.set(id, (map.get(id) ?? 0) + 1);
  }
  return [...map.entries()].map(([id, count]) => ({ id, count }));
}

/** 查询命中：name 或 tagline（任一语言值），大小写不敏感；空查询恒命中 */
export function matchQuery(a: AppIndexEntry, query: string): boolean {
  const q = (query ?? "").trim().toLowerCase();
  if (!q) return true;
  if (a.name.toLowerCase().includes(q)) return true;
  const tl = a.tagline;
  if (tl) {
    for (const v of Object.values(tl)) {
      if (typeof v === "string" && v.toLowerCase().includes(q)) return true;
    }
  }
  return false;
}

/**
 * 组合过滤：category 为 "all"/空 → 不限分类；query 为空 → 不限文本。
 * 结果保持输入顺序。
 */
export function filterApps(
  apps: AppIndexEntry[],
  query: string,
  category: string | null | undefined,
): AppIndexEntry[] {
  const want = category?.trim() ?? "";
  const all = !want || want === ALL_CATEGORIES;
  return apps.filter((a) => (all || categoryOf(a) === want) && matchQuery(a, query));
}
