/**
 * 前端区域静态兜底：`net_region()` invoke 失败时的语言/时区判定（纯函数）。
 * 背景：不得硬编码 global（国内用户会被误导到国际路线）。
 */
import { describe, expect, it } from "vitest";
import { staticRegionFallback, staticRegionOf } from "./region";

describe("staticRegionOf（语言/时区注入判定）", () => {
  it("中文语言 → cn", () => {
    expect(staticRegionOf("zh-CN", "Europe/Paris")).toBe("cn");
    expect(staticRegionOf("zh", "America/New_York")).toBe("cn");
    expect(staticRegionOf("ZH-CN", "UTC")).toBe("cn");
  });

  it("Asia/Shanghai 时区 → cn（语言英文也判国内）", () => {
    expect(staticRegionOf("en-US", "Asia/Shanghai")).toBe("cn");
  });

  it("其他 → global", () => {
    expect(staticRegionOf("en-US", "America/New_York")).toBe("global");
    expect(staticRegionOf("ja-JP", "Asia/Tokyo")).toBe("global");
    expect(staticRegionOf("", "")).toBe("global");
  });
});

describe("staticRegionFallback（浏览器环境）", () => {
  it("返回合法区域值（cn | global），不抛错", () => {
    const v = staticRegionFallback();
    expect(v === "cn" || v === "global").toBe(true);
  });
});
