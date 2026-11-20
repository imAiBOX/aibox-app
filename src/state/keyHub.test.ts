/**
 * 「一个 key 通吃」聚合与共用 key 判定。
 */
import { describe, expect, it } from "vitest";
import { keyProviderCoverage, sharedKeyProvider } from "./keyHub";

const APPS = [
  { id: "a", name: "A", keyProviders: ["openrouter", "anthropic"] },
  { id: "b", name: "B", keyProviders: ["openrouter"] },
  { id: "c", name: "C", keyProviders: ["anthropic", "openai"] },
  { id: "d", name: "D" },
];

describe("keyProviderCoverage", () => {
  it("按覆盖数降序聚合", () => {
    const cov = keyProviderCoverage(APPS);
    expect(cov[0].provider).toBe("anthropic");
    expect(cov[0].count).toBe(2);
    expect(cov.find((c) => c.provider === "openrouter")?.count).toBe(2);
  });

  it("没有 keyProviders 的应用不贡献覆盖", () => {
    const cov = keyProviderCoverage(APPS);
    expect(cov.every((c) => !c.appIds.includes("d"))).toBe(true);
  });
});

describe("sharedKeyProvider", () => {
  it("已装应用与候选共用同一 key → 返回该 provider", () => {
    expect(sharedKeyProvider({ id: "x", name: "X", keyProviders: ["openrouter"] }, APPS)).toBe(
      "openrouter",
    );
  });
  it("没有共用 → null", () => {
    expect(sharedKeyProvider({ id: "x", name: "X", keyProviders: ["deepseek"] }, APPS)).toBeNull();
    expect(sharedKeyProvider({ id: "x", name: "X" }, APPS)).toBeNull();
  });
});
