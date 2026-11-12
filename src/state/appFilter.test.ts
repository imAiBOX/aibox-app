/**
 * 目录筛选纯逻辑：空查询 / 中文查询 / 大小写 / 分类组合 / 未知分类 / 清空。
 */
import { describe, expect, it } from "vitest";
import type { AppIndexEntry } from "../net/client";
import { ALL_CATEGORIES, categoryOf, deriveCategories, filterApps, matchQuery } from "./appFilter";

function entry(p: Partial<AppIndexEntry>): AppIndexEntry {
  return {
    id: p.id ?? "x",
    name: p.name ?? "X",
    category: p.category,
    tagline: p.tagline,
    platforms: p.platforms ?? ["windows"],
    status: p.status ?? "stable",
    ...p,
  } as AppIndexEntry;
}

const apps: AppIndexEntry[] = [
  entry({
    id: "ollama",
    name: "Ollama",
    category: "local-llm",
    tagline: { en: "Run large language models locally", zh: "本地运行大模型" },
  }),
  entry({
    id: "cherry",
    name: "Cherry Studio",
    category: "chat",
    tagline: { en: "Multi-provider chat desktop app", zh: "多平台聊天客户端" },
  }),
  entry({
    id: "cline",
    name: "Cline",
    category: "coding",
    tagline: { en: "AI coding agent in VS Code", zh: "VS Code 里的 AI 编程代理" },
  }),
  entry({ id: "mystery", name: "Mystery", category: "brain", tagline: { en: "undocumented" } }),
  entry({ id: "nocat", name: "NoCategory" }),
];

describe("空查询 / 清空", () => {
  it("空查询 + all → 全量返回（顺序不变）", () => {
    expect(filterApps(apps, "", ALL_CATEGORIES)).toHaveLength(apps.length);
    expect(filterApps(apps, "   ", ALL_CATEGORIES).map((a) => a.id)).toEqual([
      "ollama",
      "cherry",
      "cline",
      "mystery",
      "nocat",
    ]);
  });
  it("清空（回到空查询 + all）恢复全量", () => {
    const filtered = filterApps(apps, "cline", "coding");
    expect(filtered).toHaveLength(1);
    expect(filterApps(apps, "", ALL_CATEGORIES)).toHaveLength(apps.length);
  });
  it("query 为 null/undefined 容错", () => {
    expect(filterApps(apps, null as unknown as string, ALL_CATEGORIES)).toHaveLength(apps.length);
  });
});

describe("查询匹配", () => {
  it("大小写不敏感（name）", () => {
    expect(matchQuery(apps[0]!, "OLLAMA")).toBe(true);
    expect(matchQuery(apps[1]!, "cherry studio")).toBe(true);
    expect(matchQuery(apps[1]!, "CHERRY")).toBe(true);
  });
  it("中文查询命中 zh tagline", () => {
    expect(matchQuery(apps[0]!, "大模型")).toBe(true);
    expect(matchQuery(apps[2]!, "编程代理")).toBe(true);
  });
  it("英文查询命中 en tagline", () => {
    expect(matchQuery(apps[2]!, "VS Code")).toBe(true);
  });
  it("只查 name/tagline，不查 id（id 命中不了）", () => {
    expect(matchQuery(apps[0]!, "ollama")).toBe(true); // name 本身含该串
    expect(matchQuery(entry({ id: "hidden-id", name: "Foo", tagline: { en: "Bar" } }), "hidden-id")).toBe(
      false,
    );
  });
  it("trim 后匹配", () => {
    expect(matchQuery(apps[0]!, "  Ollama  ")).toBe(true);
  });
});

describe("分类过滤与组合", () => {
  it("按分类过滤", () => {
    expect(filterApps(apps, "", "chat").map((a) => a.id)).toEqual(["cherry"]);
    expect(filterApps(apps, "", "coding").map((a) => a.id)).toEqual(["cline"]);
  });
  it("分类 + 查询组合（先分类后搜索）", () => {
    expect(filterApps(apps, "cherry", "coding")).toHaveLength(0);
    expect(filterApps(apps, "cherry", "chat").map((a) => a.id)).toEqual(["cherry"]);
    expect(filterApps(apps, "大模型", ALL_CATEGORIES).map((a) => a.id)).toEqual(["ollama"]);
    expect(filterApps(apps, "大模型", "coding")).toHaveLength(0);
  });
  it("未知分类 id → 空结果（不回落到全部）", () => {
    expect(filterApps(apps, "", "no-such-category")).toHaveLength(0);
  });
  it("缺 category 的应用归入 other 分类", () => {
    expect(filterApps(apps, "", "other").map((a) => a.id)).toEqual(["nocat"]);
    expect(categoryOf(entry({ id: "blank", name: "B", category: "  " }))).toBe("other");
  });
  it("null/空分类等价于 all", () => {
    expect(filterApps(apps, "", null)).toHaveLength(apps.length);
    expect(filterApps(apps, "", "")).toHaveLength(apps.length);
  });
});

describe("deriveCategories（数据推导，不硬编码）", () => {
  it("去重 + 计数 + 首次出现顺序", () => {
    expect(deriveCategories(apps)).toEqual([
      { id: "local-llm", count: 1 },
      { id: "chat", count: 1 },
      { id: "coding", count: 1 },
      { id: "brain", count: 1 },
      { id: "other", count: 1 },
    ]);
  });
  it("未知分类 id 原样保留（显示层回落原样展示）", () => {
    const ids = deriveCategories(apps).map((c) => c.id);
    expect(ids).toContain("brain");
  });
  it("计数随查询收窄（供 chip 计数用）", () => {
    const searchFiltered = filterApps(apps, "", ALL_CATEGORIES);
    expect(deriveCategories(searchFiltered).find((c) => c.id === "coding")?.count).toBe(1);
    const narrowed = filterApps(apps, "cherry", ALL_CATEGORIES);
    expect(deriveCategories(narrowed)).toEqual([{ id: "chat", count: 1 }]);
  });
  it("空列表 → 空 chips", () => {
    expect(deriveCategories([])).toEqual([]);
  });
});
