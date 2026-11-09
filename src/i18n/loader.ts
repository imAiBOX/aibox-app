/**
 * registry locales 运行时热加载。
 * fetch `locales/<lang>.json`（相对 base，亦可指 registry raw 源），
 * 失败回落内置 en；缓存 12h 语义（localStorage 时间戳 + 内存 Map 双层）。
 */
export const LOCALES_BASE = "locales";
export const CACHE_TTL_MS = 12 * 60 * 60 * 1000; // 12h

const LS_PREFIX = "aibox.locales.at.";

export interface LocaleCacheEntry {
  data: Record<string, string>;
  at: number;
}

const mem = new Map<string, LocaleCacheEntry>();

function cacheKey(lang: string): string {
  return LS_PREFIX + lang;
}

/** 读缓存（内存优先，其次 localStorage 时间戳校验）；过期返回 null */
export function readCache(lang: string, now = Date.now()): Record<string, string> | null {
  const m = mem.get(lang);
  if (m && now - m.at < CACHE_TTL_MS) return m.data;
  try {
    const raw = localStorage.getItem(cacheKey(lang));
    if (raw) {
      const entry = JSON.parse(raw) as LocaleCacheEntry;
      if (now - entry.at < CACHE_TTL_MS) {
        mem.set(lang, entry);
        return entry.data;
      }
    }
  } catch {
    /* localStorage 不可用/损坏 → 视为无缓存 */
  }
  return null;
}

export function writeCache(lang: string, data: Record<string, string>, now = Date.now()): void {
  mem.set(lang, { data, at: now });
  try {
    localStorage.setItem(cacheKey(lang), JSON.stringify({ data, at: now }));
  } catch {
    /* 只留内存缓存 */
  }
}

export function clearCache(lang?: string): void {
  if (lang) {
    mem.delete(lang);
    try {
      localStorage.removeItem(cacheKey(lang));
    } catch {
      /* ignore */
    }
  } else {
    mem.clear();
    try {
      for (let i = localStorage.length - 1; i >= 0; i--) {
        const k = localStorage.key(i);
        if (k?.startsWith(LS_PREFIX)) localStorage.removeItem(k);
      }
    } catch {
      /* ignore */
    }
  }
}

/** 拍平嵌套 key：{a:{b:"x"}} → {"a.b":"x"}（与 check-i18n.mjs 同算法） */
export function flatten(
  obj: Record<string, unknown>,
  prefix = "",
  out: Record<string, string> = {},
): Record<string, string> {
  for (const [k, v] of Object.entries(obj)) {
    const key = prefix ? `${prefix}.${k}` : k;
    if (v && typeof v === "object" && !Array.isArray(v)) {
      flatten(v as Record<string, unknown>, key, out);
    } else {
      out[key] = String(v);
    }
  }
  return out;
}

/**
 * 热加载语言包。失败（网络/404/坏 JSON）→ 抛错由调用方回落内置 en。
 * force=true 绕过缓存（「检查更新」语义）。
 */
export async function loadRemoteLocales(
  lang: string,
  opts: { base?: string; force?: boolean; fetchImpl?: typeof fetch } = {},
): Promise<Record<string, string>> {
  const base = opts.base ?? LOCALES_BASE;
  const f = opts.fetchImpl ?? fetch;
  if (!opts.force) {
    const cached = readCache(lang);
    if (cached) return cached;
  }
  const res = await f(`${base}/${lang}.json`, { cache: "no-store" });
  if (!res.ok) throw new Error(`locales ${lang}: HTTP ${res.status}`);
  const data = (await res.json()) as Record<string, unknown>;
  const flat = flatten(data);
  writeCache(lang, flat);
  return flat;
}

/**
 * 尝试热加载并合入 i18next；任何失败静默回落内置 en（不阻塞启动）。
 * 返回是否成功（便于测试断言）。
 */
export async function hotLoadInto(
  i18n: {
    addResourceBundle: (
      lng: string,
      ns: string,
      resources: Record<string, unknown>,
      deep?: boolean,
      overwrite?: boolean,
    ) => unknown;
  },
  lang: string,
  opts: { base?: string; force?: boolean; fetchImpl?: typeof fetch } = {},
): Promise<boolean> {
  try {
    const flat = await loadRemoteLocales(lang, opts);
    // i18next 深层资源：把拍平 key 再还原为嵌套（deep merge 用点路径即可）
    i18n.addResourceBundle(lang, "translation", toNested(flat), true, true);
    return true;
  } catch {
    return false;
  }
}

/** 拍平 key 还原嵌套对象（loadRemoteLocales 的逆变换） */
export function toNested(flat: Record<string, string>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(flat)) {
    const parts = k.split(".");
    let cur = out;
    for (let i = 0; i < parts.length - 1; i++) {
      const p = parts[i];
      if (typeof cur[p] !== "object" || cur[p] === null) cur[p] = {};
      cur = cur[p] as Record<string, unknown>;
    }
    cur[parts[parts.length - 1]] = v;
  }
  return out;
}
