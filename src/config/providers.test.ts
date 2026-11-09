/**
 * C3 key 校验纯函数：各 provider 正反例 + 人话纠错分支。
 * key 全部为明显假值（sk-test-…），绝不使用任何真实 key。
 */
import { describe, expect, it } from "vitest";
import {
  PROVIDERS,
  fallbackModels,
  maskKey,
  metaFor,
  priceHintKey,
  providerById,
  validateKey,
} from "./providers";

const openrouter = providerById("openrouter")!;
const anthropic = providerById("anthropic")!;
const openai = providerById("openai")!;
const google = providerById("google")!;

// registry keyFormat（与 aibox-registry app.json 一致）
const FMT = {
  openrouter: "^sk-or-v1-[A-Za-z0-9]{64}$",
  anthropic: "^sk-ant-[A-Za-z0-9_-]{40,}$",
  openai: "^sk-(proj-)?[A-Za-z0-9_-]{20,}$",
  google: "^AIza[0-9A-Za-z_-]{35}$",
};

// 明显假 key（仅测试用）
const KEY = {
  openrouter: `sk-or-v1-${"a".repeat(64)}`,
  anthropic: `sk-ant-${"b".repeat(40)}`,
  openai: `sk-${"c".repeat(24)}`,
  openaiProj: `sk-proj-${"c".repeat(24)}`,
  google: `AIza${"d".repeat(35)}`,
};

describe("validateKey 各 provider 正例（registry keyFormat）", () => {
  it("openrouter：sk-or-v1- + 64 位", () => {
    expect(validateKey(openrouter, KEY.openrouter, FMT.openrouter)).toEqual({ ok: true });
  });
  it("anthropic：sk-ant- + ≥40 位", () => {
    expect(validateKey(anthropic, KEY.anthropic, FMT.anthropic)).toEqual({ ok: true });
  });
  it("openai：sk- 与 sk-proj- 均可", () => {
    expect(validateKey(openai, KEY.openai, FMT.openai)).toEqual({ ok: true });
    expect(validateKey(openai, KEY.openaiProj, FMT.openai)).toEqual({ ok: true });
  });
  it("google：AIza + 35 位", () => {
    expect(validateKey(google, KEY.google, FMT.google)).toEqual({ ok: true });
  });
});

describe("validateKey 反例与人话纠错", () => {
  it("空 → config.key.empty", () => {
    expect(validateKey(openrouter, "   ", FMT.openrouter).msgKey).toBe("config.key.empty");
  });
  it("粘了整个网页（URL）→ pastedPage", () => {
    expect(
      validateKey(openrouter, "https://openrouter.ai/keys sk-or-v1-xxx", FMT.openrouter).msgKey,
    ).toBe("config.key.pastedPage");
  });
  it("粘了整个网页（含换行/HTML）→ pastedPage", () => {
    expect(validateKey(openrouter, "<html>\n<body>key</body>", FMT.openrouter).msgKey).toBe(
      "config.key.pastedPage",
    );
    expect(validateKey(openrouter, `sk-or-v1-${"a".repeat(30)}\nextra`, FMT.openrouter).msgKey).toBe(
      "config.key.pastedPage",
    );
  });
  it("太短 → tooShort", () => {
    expect(validateKey(openrouter, "sk-or-v1-abc", FMT.openrouter).msgKey).toBe(
      "config.key.tooShort",
    );
  });
  it("前缀不对 → wrongPrefix（带 {{prefix}} 参数）", () => {
    const r = validateKey(openrouter, KEY.anthropic, FMT.openrouter);
    expect(r.msgKey).toBe("config.key.wrongPrefix");
    expect(r.params?.prefix).toBe("sk-or-v1-");
  });
  it("前缀对但格式不符 → badFormat", () => {
    // sk-or-v1- 前缀正确、长度 ≥16 但不足 64
    const r = validateKey(openrouter, `sk-or-v1-${"a".repeat(20)}`, FMT.openrouter);
    expect(r.msgKey).toBe("config.key.badFormat");
  });
  it("无 registry keyFormat → 本地 pattern 回落", () => {
    expect(validateKey(openrouter, KEY.openrouter)).toEqual({ ok: true });
    expect(validateKey(openrouter, "sk-ant-nope-nope-nope").ok).toBe(false);
  });
  it("registry 给了坏正则 → 不炸，回落本地 pattern", () => {
    expect(validateKey(openrouter, KEY.openrouter, "([")).toEqual({ ok: true });
  });
});

describe("辅助纯函数", () => {
  it("maskKey：sk-**** 形式（硬规则）", () => {
    expect(maskKey(KEY.openrouter)).toBe(`sk-****${KEY.openrouter.slice(-4)}`);
    expect(maskKey("short")).toBe("****");
  });
  it("priceHintKey：registry 中文短语映射，未知原样", () => {
    expect(priceHintKey("低")).toBe("s5.price.low");
    expect(priceHintKey("已含订阅")).toBe("s5.price.included");
    expect(priceHintKey("随机")).toBeNull();
  });
  it("fallbackModels：registry 缺席时按 modelIds 兜底", () => {
    expect(fallbackModels(openrouter).length).toBe(3);
    expect(fallbackModels(google)).toEqual([]);
  });
  it("metaFor：只取指定 provider 的 registry 元数据", () => {
    const metas = { openrouter: { keyFormat: FMT.openrouter } };
    expect(metaFor(metas, "openrouter")?.keyFormat).toBe(FMT.openrouter);
    expect(metaFor(metas, "anthropic")).toBeUndefined();
    expect(metaFor(undefined, "openrouter")).toBeUndefined();
  });
  it("六路径构成：3 完整 3 TODO，仅 openrouter 推荐", () => {
    expect(PROVIDERS).toHaveLength(6);
    expect(PROVIDERS.filter((p) => p.status === "full")).toHaveLength(3);
    expect(PROVIDERS.filter((p) => p.status === "todo")).toHaveLength(3);
    expect(PROVIDERS.filter((p) => p.recommended).map((p) => p.id)).toEqual(["openrouter"]);
  });
});
