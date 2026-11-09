/**
 * CONFIG 六路径数据驱动。
 * 3 完整（openrouter/anthropic/openai）+ 3 TODO 占位（google/claude-oauth/ollama）。
 * 视觉等权重；推荐徽章是右上角小标签（C1）。
 *
 * C3 key 格式：优先用 registry modelProviders.keyFormat（真数据，经 registry_app
 * 传入），回落本文件的本地 keyPattern（registry 拉不到时的兜底）。
 * C4 模型清单：registry recommendedModels 真数据，fallbackModels() 兜底。
 */
import type { ModelOption, ProviderMeta } from "./types";

export type { ModelOption, ProviderMeta };
export type ProviderStatus = "full" | "todo";

export interface ProviderPath {
  id: string;
  nameKey: string;
  descKey: string;
  /** 推荐标签 = 联盟位，需明示返佣（C1①） */
  recommended?: boolean;
  status: ProviderStatus;
  /** key 输入占位与校验（C3）；OAuth/本地路径无 key */
  keyPlaceholder?: string;
  /** 本地 keyPattern 回落（registry keyFormat 缺席时用） */
  keyPattern?: RegExp;
  /** 一句话取舍说明 key（C4 列表头） */
  modelHintKey?: string;
  /** 本地模型 id 回落（registry recommendedModels 缺席时用） */
  modelIds: string[];
  /** 官网入口（C2；registry getKeyUrl 优先，此处为兜底） */
  siteUrl?: string;
  /** 无 key 路线（OAuth / 本地） */
  noKey?: boolean;
}

export const PROVIDERS: ProviderPath[] = [
  {
    id: "openrouter",
    nameKey: "config.p.openrouter.name",
    descKey: "config.p.openrouter.desc",
    recommended: true,
    status: "full",
    keyPlaceholder: "sk-or-v1-…",
    keyPattern: /^sk-or-v1-[A-Za-z0-9_-]{8,}$/,
    modelIds: ["anthropic/claude-sonnet-4", "openai/gpt-5", "google/gemini-2.5-flash"],
    modelHintKey: "config.p.openrouter.models",
    siteUrl: "https://openrouter.ai/keys",
  },
  {
    id: "anthropic",
    nameKey: "config.p.anthropic.name",
    descKey: "config.p.anthropic.desc",
    status: "full",
    keyPlaceholder: "sk-ant-…",
    keyPattern: /^sk-ant-[A-Za-z0-9_-]{8,}$/,
    modelIds: ["claude-sonnet-4-5", "claude-haiku-4-5"],
    modelHintKey: "config.p.anthropic.models",
    siteUrl: "https://console.anthropic.com/settings/keys",
  },
  {
    id: "openai",
    nameKey: "config.p.openai.name",
    descKey: "config.p.openai.desc",
    status: "full",
    keyPlaceholder: "sk-…",
    keyPattern: /^sk-(proj-)?[A-Za-z0-9_-]{16,}$/,
    modelIds: ["gpt-5.1", "gpt-5-mini"],
    modelHintKey: "config.p.openai.models",
    siteUrl: "https://platform.openai.com/api-keys",
  },
  {
    id: "google",
    nameKey: "config.p.google.name",
    descKey: "config.p.google.desc",
    status: "todo",
    keyPlaceholder: "AIza…",
    keyPattern: /^AIza[0-9A-Za-z_-]{35}$/,
    modelIds: [],
    siteUrl: "https://aistudio.google.com/apikey",
  },
  {
    id: "claude-oauth",
    nameKey: "config.p.claudeOAuth.name",
    descKey: "config.p.claudeOAuth.desc",
    status: "todo",
    noKey: true,
    modelIds: [],
    siteUrl: "https://claude.ai/",
  },
  {
    id: "ollama",
    nameKey: "config.p.ollama.name",
    descKey: "config.p.ollama.desc",
    status: "todo",
    noKey: true,
    modelIds: [],
    siteUrl: "https://ollama.com/download",
  },
];

export function providerById(id: string | null): ProviderPath | null {
  if (!id) return null;
  return PROVIDERS.find((p) => p.id === id) ?? null;
}

export interface KeyCheck {
  ok: boolean;
  msgKey?: string;
  params?: Record<string, string>;
}

/**
 * C3 即时格式校验。
 * 规则顺序：空 → 粘了整个网页/含换行 → 太短 → keyFormat（registry 优先）。
 * 错误全部人话（「你粘的像是复制了整个网页」这类）。
 *
 * @param keyFormatSrc registry modelProviders.keyFormat（正则源串），缺席回落 p.keyPattern
 */
export function validateKey(
  p: ProviderPath,
  raw: string,
  keyFormatSrc?: string,
): KeyCheck {
  const v = raw.trim();
  if (!v) return { ok: false, msgKey: "config.key.empty" };
  // 粘了整个网页：含内部空白 / 超长 / 像 URL / 像 HTML
  if (v.length > 200 || /\s/.test(v) || /https?:\/\//i.test(v) || /<[a-z/!]/i.test(v)) {
    return { ok: false, msgKey: "config.key.pastedPage" };
  }
  if (v.length < 16) return { ok: false, msgKey: "config.key.tooShort" };
  const src = keyFormatSrc ?? p.keyPattern?.source;
  if (src) {
    let re: RegExp | null = null;
    try {
      re = new RegExp(src);
    } catch {
      re = p.keyPattern ?? null; // registry 给了坏正则 → 本地回落，不炸 UI
    }
    if (re && !re.test(v)) {
      const m = /^\^([A-Za-z0-9_-]+)/.exec(src);
      if (m && !v.startsWith(m[1])) {
        return { ok: false, msgKey: "config.key.wrongPrefix", params: { prefix: m[1] } };
      }
      return { ok: false, msgKey: "config.key.badFormat" };
    }
  }
  return { ok: true };
}

/** registry recommendedModels 缺席时的兜底（mock 兜底同源：名字=id） */
export function fallbackModels(p: ProviderPath): ModelOption[] {
  return p.modelIds.map((id) => ({ id, name: id }));
}

/** 从 manifest.modelProviders 取该 provider 的 registry 元数据（可空） */
export function metaFor(
  metas: Record<string, ProviderMeta> | undefined,
  providerId: string,
): ProviderMeta | undefined {
  return metas?.[providerId];
}

/** C4 priceHint 是 registry 中文短语 → i18n key 映射（未知值原样显示） */
export function priceHintKey(hint: string | undefined): string | null {
  switch (hint) {
    case "低":
      return "s5.price.low";
    case "中等":
      return "s5.price.mid";
    case "偏高":
      return "s5.price.high";
    case "免费":
      return "s5.price.free";
    case "已含订阅":
      return "s5.price.included";
    default:
      return null;
  }
}

/** key 脱敏（硬规则：日志/诊断一律 sk-****1234 形式） */
export function maskKey(raw: string): string {
  const v = raw.trim();
  if (v.length <= 8) return "****";
  return `${v.slice(0, 3)}****${v.slice(-4)}`;
}
