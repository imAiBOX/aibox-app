/**
 * CONFIG 六路径数据驱动。
 * 3 完整（openrouter/anthropic/openai）+ 3 TODO 占位（google/claude-oauth/ollama）。
 * 视觉等权重；推荐徽章是右上角小标签。
 */
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
  keyPattern?: RegExp;
  /** 推荐模型短清单（C4）：registry 提供，此处为骨架占位 id */
  modelIds: string[];
  /** C4 一句话取舍说明 key */
  modelHintKey?: string;
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
    modelIds: ["auto", "cheap", "smart"],
    modelHintKey: "config.p.openrouter.models",
  },
  {
    id: "anthropic",
    nameKey: "config.p.anthropic.name",
    descKey: "config.p.anthropic.desc",
    status: "full",
    keyPlaceholder: "sk-ant-…",
    keyPattern: /^sk-ant-[A-Za-z0-9_-]{8,}$/,
    modelIds: ["claude-sonnet", "claude-haiku", "claude-opus"],
    modelHintKey: "config.p.anthropic.models",
  },
  {
    id: "openai",
    nameKey: "config.p.openai.name",
    descKey: "config.p.openai.desc",
    status: "full",
    keyPlaceholder: "sk-…",
    keyPattern: /^sk-[A-Za-z0-9_-]{16,}$/,
    modelIds: ["gpt-5-mini", "gpt-5", "gpt-4o-mini"],
    modelHintKey: "config.p.openai.models",
  },
  {
    id: "google",
    nameKey: "config.p.google.name",
    descKey: "config.p.google.desc",
    status: "todo",
    noKey: false,
    keyPlaceholder: "AIza…",
    modelIds: [],
  },
  {
    id: "claude-oauth",
    nameKey: "config.p.claudeOAuth.name",
    descKey: "config.p.claudeOAuth.desc",
    status: "todo",
    noKey: true,
    modelIds: [],
  },
  {
    id: "ollama",
    nameKey: "config.p.ollama.name",
    descKey: "config.p.ollama.desc",
    status: "todo",
    noKey: true,
    modelIds: [],
  },
];

export function providerById(id: string | null): ProviderPath | null {
  if (!id) return null;
  return PROVIDERS.find((p) => p.id === id) ?? null;
}

/** C3 即时格式校验（错误给「你粘的像是复制了整个网页」人话纠错，C3） */
export function validateKey(
  p: ProviderPath,
  raw: string,
): { ok: boolean; msgKey?: string } {
  const v = raw.trim();
  if (!v) return { ok: false, msgKey: "config.key.empty" };
  if (v.length > 200 || /\s/.test(v)) return { ok: false, msgKey: "config.key.pastedPage" };
  if (p.keyPattern && !p.keyPattern.test(v)) return { ok: false, msgKey: "config.key.badFormat" };
  return { ok: true };
}

/** key 脱敏（硬规则：日志/SQLite/诊断一律 sk-****1234 形式） */
export function maskKey(raw: string): string {
  const v = raw.trim();
  if (v.length <= 8) return "****";
  return `${v.slice(0, 3)}****${v.slice(-4)}`;
}
