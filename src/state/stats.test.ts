/**
 * 卡片计数纯逻辑：formatCount 紧凑格式化 + appCounts 取数/零值隐藏。
 */
import { describe, expect, it } from "vitest";
import { appCounts, formatCount } from "./stats";
import type { StatsPayload } from "../net/client";

describe("formatCount", () => {
  it("小于千直接给整数", () => {
    expect(formatCount(0)).toBe("0");
    expect(formatCount(1)).toBe("1");
    expect(formatCount(999)).toBe("999");
  });

  it("千级 → k（一位小数，去掉 .0）", () => {
    expect(formatCount(1000)).toBe("1k");
    expect(formatCount(1234)).toBe("1.2k");
    expect(formatCount(9999)).toBe("10k");
    expect(formatCount(12_500)).toBe("12.5k");
    expect(formatCount(999_999)).toBe("1000k");
  });

  it("百万级 → M", () => {
    expect(formatCount(1_000_000)).toBe("1M");
    expect(formatCount(1_250_000)).toBe("1.3M");
    expect(formatCount(2_000_000)).toBe("2M");
  });

  it("非法/负数 → 0", () => {
    expect(formatCount(Number.NaN)).toBe("0");
    expect(formatCount(Number.POSITIVE_INFINITY)).toBe("0");
    expect(formatCount(-5)).toBe("0");
  });
});

describe("appCounts", () => {
  const stats: StatsPayload = {
    apps: {
      openclaw: { views: 4210, installs: 863 },
      ghost: { views: 0, installs: 0 },
    },
    updatedAt: "t",
  };

  it("命中 → 计数", () => {
    expect(appCounts(stats, "openclaw")).toEqual({ views: 4210, installs: 863 });
  });

  it("缺条目 / null stats → null（不显示）", () => {
    expect(appCounts(stats, "nope")).toBeNull();
    expect(appCounts(null, "openclaw")).toBeNull();
    expect(appCounts(undefined, "openclaw")).toBeNull();
  });

  it("全零 → null（零噪音不渲染）", () => {
    expect(appCounts(stats, "ghost")).toBeNull();
  });

  it("容错：非数字字段归 0", () => {
    const junk = { apps: { a: { views: Number.NaN, installs: 3 } } } as unknown as StatsPayload;
    expect(appCounts(junk, "a")).toEqual({ views: 0, installs: 3 });
  });
});
