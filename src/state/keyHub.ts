/**
 * 「一个 key 通吃」：把 registry 声明的 **keyProviders** 聚合成「一个 key 能用多少个应用」。
 *
 * **这是本产品与「一堆应用各自要 key」的根本差异**：
 * 用户配一次 OpenRouter/Anthropic/OpenAI 的 key，就能装好一大批 AI 应用，
 * 不用为每个应用重新注册、重新充值、重新配一遍。
 *
 * 数据源：registry index.json 每个条目的 `keyProviders`（app.json 里 authType=api-key 的服务商）。
 * 纯函数，可单测。
 */

export interface KeyHubApp {
  id: string;
  name: string;
  /** 该应用支持且需要 key 的服务商 */
  keyProviders?: string[];
}

export interface KeyProviderCoverage {
  provider: string;
  /** 支持该 key 的应用 id（按目录顺序） */
  appIds: string[];
  /** 覆盖数量 */
  count: number;
}

/** 聚合：每个 key 服务商能通吃多少个应用（按覆盖数降序） */
export function keyProviderCoverage(apps: KeyHubApp[]): KeyProviderCoverage[] {
  const map = new Map<string, string[]>();
  for (const a of apps) {
    for (const p of a.keyProviders ?? []) {
      const list = map.get(p) ?? [];
      list.push(a.id);
      map.set(p, list);
    }
  }
  return [...map.entries()]
    .map(([provider, appIds]) => ({ provider, appIds, count: appIds.length }))
    .sort((a, b) => (b.count - a.count) || a.provider.localeCompare(b.provider));
}

/** 覆盖数达到这个值才值得当作「一个 key 通吃」来宣传（低于它就是普通分类） */
export const KEY_HUB_MIN_COVERAGE = 3;

/**
 * 两个应用是否共用同一个 key（都用 api-key 型服务商）。
 * 返回共用的 provider id（没有则 null）—— 用于推荐理由「和 X 共用一个 key」。
 */
export function sharedKeyProvider(
  candidate: KeyHubApp,
  installed: KeyHubApp[],
): string | null {
  const mine = new Set(candidate.keyProviders ?? []);
  if (mine.size === 0) return null;
  for (const inst of installed) {
    for (const p of inst.keyProviders ?? []) {
      if (mine.has(p)) return p;
    }
  }
  return null;
}
