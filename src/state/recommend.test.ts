/**
 * 推荐排序测试（纯函数）。
 */
import { describe, expect, it } from "vitest";
import { recommendApps, type CatalogApp } from "./recommend";

const CATALOG: CatalogApp[] = [
  { id: "a", name: "A", category: "chat", difficulty: 1, popularity: 3, status: "stable", platforms: ["windows", "macos"] },
  { id: "b", name: "B", category: "chat", difficulty: 2, popularity: 5, status: "stable", platforms: ["windows", "macos"] },
  { id: "c", name: "C", category: "coding", difficulty: 3, popularity: 5, status: "stable", platforms: ["windows"] },
  { id: "d", name: "D", category: "coding", difficulty: 1, popularity: 1, status: "stable", platforms: ["windows", "macos"] },
  { id: "e", name: "E", category: "media", difficulty: 2, popularity: 4, status: "beta", platforms: ["windows", "macos"] },
];

describe("recommendApps", () => {
  it("已装的推荐同分类优先", () => {
    const r = recommendApps(CATALOG, { installedIds: ["x-chat"], limit: 2 });
    // 已装 a（chat）→ 同分类的 b 应该排前面
    expect(r[0].id).toBe("b");
  });

  it("已装的不再推荐", () => {
    const r = recommendApps(CATALOG, { installedIds: ["a"], limit: 5 });
    expect(r.map((x) => x.id)).not.toContain("a");
  });

  it("没装任何东西 → 纯按热度+难度（第一次打开给最值得装的）", () => {
    const r = recommendApps(CATALOG, { installedIds: [], limit: 3 });
    expect(r[0].id).toBe("b"); // 热度 5、难度 2
  });

  it("非 stable 的不推（还没就绪的别骚扰）", () => {
    const r = recommendApps(CATALOG, { installedIds: [], limit: 5 });
    expect(r.map((x) => x.id)).not.toContain("e");
  });

  it("平台不符的不推", () => {
    const r = recommendApps(CATALOG, { installedIds: [], limit: 5, platform: "macos" });
    expect(r.map((x) => x.id)).not.toContain("c");
  });

  it("全装完 → 空（不硬凑）", () => {
    const r = recommendApps(CATALOG, { installedIds: ["a", "b", "c", "d", "e"], limit: 3 });
    expect(r).toEqual([]);
  });

  it("limit 生效", () => {
    expect(recommendApps(CATALOG, { installedIds: [], limit: 2 })).toHaveLength(2);
  });
});

describe("共用 key 的推荐权重更高", () => {
  const apps: CatalogApp[] = [
    { id: "inst", name: "Inst", category: "chat", difficulty: 1, popularity: 5, status: "stable" },
    { id: "sameKey", name: "SameKey", category: "coding", difficulty: 3, popularity: 1, status: "stable", keyProviders: ["openrouter"] },
    { id: "sameCat", name: "SameCat", category: "chat", difficulty: 2, popularity: 2, status: "stable" },
  ];
  it("共用 key 的排到同分类之前", () => {
    const r = recommendApps(apps, {
      installedIds: ["inst"],
      limit: 2,
      installedKeyProviders: ["openrouter"],
    });
    expect(r[0].id).toBe("sameKey");
  });
});
