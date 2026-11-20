/**
 * 个性化推荐（纯函数，可单测）。
 *
 * **为什么做**：目录从 29 个往 50+ 走之后，「共 29 个应用」的新手会在列表里迷路。
 * 推荐的价值不是「再列几个应用」，而是**把「你可能正好需要的」放到最前面**。
 *
 * 排序依据（都来自 registry 真实字段，不自造）：
 * - **同分类**（用户已装的某个应用属于该分类）→ 强信号（+10）
 * - **popularity**（registry 维护的热度，0-5）→ 次信号（+popularity）
 * - 同分时：**难度低的排前面**（新手优先），再按名称稳定排序
 *
 * 边界：
 * - 什么都没装 → 纯按热度+难度推（第一次打开就给最值得装的）
 * - 全都装过了 → 返回空（不硬凑）
 */

export interface CatalogApp {
  id: string;
  name: string;
  category?: string;
  difficulty?: number;
  popularity?: number;
  status?: string;
  /** 平台列表（如 ["windows","macos"]）；空/缺省 = 不限平台 */
  platforms?: string[];
}

export interface RecommendOptions {
  /** 已装应用 id 集合 */
  installedIds: string[];
  /** 最多推荐几个 */
  limit?: number;
  /** 平台过滤（只推当前平台能装的） */
  platform?: "windows" | "macos";
}

export function recommendApps<T extends CatalogApp>(
  catalog: T[],
  opts: RecommendOptions,
): T[] {
  const limit = opts.limit ?? 4;
  if (catalog.length === 0 || limit <= 0) return [];
  const installed = new Set(opts.installedIds);
  const installedCategories = new Set(
    catalog.filter((a) => installed.has(a.id)).map((a) => a.category ?? ""),
  );

  const candidates = catalog.filter((a) => {
    if (installed.has(a.id)) return false;
    // 只推已就绪（stable / 可安装）的
    if (a.status && a.status !== "stable") return false;
    // 平台不符的不推（装了也用不了）
    if (opts.platform && a.platforms?.length && !a.platforms.includes(opts.platform)) return false;
    return true;
  });
  if (candidates.length === 0) return [];

  const score = (a: CatalogApp): number => {
    const sameCategory = a.category && installedCategories.has(a.category) ? 10 : 0;
    const popularity = a.popularity ?? 0;
    const difficulty = a.difficulty ?? 3;
    // 同分类 + 热度 - 难度惩罚（新手优先）
    return sameCategory + popularity - difficulty * 0.5;
  };

  return [...candidates]
    .sort((a, b) => {
      const d = score(b) - score(a);
      if (d !== 0) return d;
      const da = a.difficulty ?? 3;
      const db = b.difficulty ?? 3;
      if (da !== db) return da - db;
      const pa = a.popularity ?? 0;
      const pb = b.popularity ?? 0;
      if (pa !== pb) return pb - pa;
      return a.name.localeCompare(b.name);
    })
    .slice(0, limit);
}
