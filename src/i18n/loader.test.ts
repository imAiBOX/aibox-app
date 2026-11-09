/**
 * i18n loader 测试：热加载、12h 缓存、失败回落。
 * fetch 用注入 mock，不依赖网络。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  CACHE_TTL_MS,
  clearCache,
  flatten,
  hotLoadInto,
  loadRemoteLocales,
  readCache,
  toNested,
  writeCache,
} from "./loader";

afterEach(() => {
  clearCache();
  vi.unstubAllGlobals();
});

const flat = { "a.b": "x", "a.c": "y", top: "z" };

function jsonResponse(data: unknown, ok = true, status = 200): Response {
  return {
    ok,
    status,
    json: async () => data,
  } as unknown as Response;
}

describe("flatten / toNested", () => {
  it("拍平嵌套对象", () => {
    expect(flatten({ a: { b: "x", c: { d: "y" } }, top: "z" })).toEqual({
      "a.b": "x",
      "a.c.d": "y",
      top: "z",
    });
  });
  it("toNested 是 flatten 的逆变换", () => {
    const nested = { a: { b: "x", c: { d: "y" } }, top: "z" };
    expect(toNested(flatten(nested))).toEqual(nested);
    expect(flatten(toNested(flat))).toEqual(flat);
  });
});

describe("loadRemoteLocales", () => {
  it("成功：拉取 + 拍平 + 写缓存", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(jsonResponse({ a: { b: "x" } }));
    const data = await loadRemoteLocales("zh-CN", { fetchImpl });
    expect(data).toEqual({ "a.b": "x" });
    expect(fetchImpl).toHaveBeenCalledWith("locales/zh-CN.json", { cache: "no-store" });
    expect(readCache("zh-CN")).toEqual({ "a.b": "x" });
  });

  it("12h 缓存命中：不发请求", async () => {
    writeCache("en", flat);
    const fetchImpl = vi.fn();
    const data = await loadRemoteLocales("en", { fetchImpl });
    expect(data).toEqual(flat);
    expect(fetchImpl).not.toHaveBeenCalled();
  });

  it("缓存过期（>12h）→ 重新拉取", async () => {
    writeCache("en", flat, Date.now() - CACHE_TTL_MS - 1000);
    expect(readCache("en")).toBeNull();
    const fetchImpl = vi.fn().mockResolvedValue(jsonResponse({ a: { b: "new" } }));
    const data = await loadRemoteLocales("en", { fetchImpl });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(data).toEqual({ "a.b": "new" });
  });

  it("force=true 绕过缓存", async () => {
    writeCache("en", flat);
    const fetchImpl = vi.fn().mockResolvedValue(jsonResponse({ a: { b: "fresh" } }));
    const data = await loadRemoteLocales("en", { fetchImpl, force: true });
    expect(fetchImpl).toHaveBeenCalledTimes(1);
    expect(data).toEqual({ "a.b": "fresh" });
  });

  it("HTTP 失败 → 抛错（调用方回落内置 en）", async () => {
    const fetchImpl = vi.fn().mockResolvedValue(jsonResponse({}, false, 404));
    await expect(loadRemoteLocales("xx", { fetchImpl })).rejects.toThrow("HTTP 404");
    expect(readCache("xx")).toBeNull();
  });

  it("网络异常 → 抛错", async () => {
    const fetchImpl = vi.fn().mockRejectedValue(new Error("offline"));
    await expect(loadRemoteLocales("en", { fetchImpl })).rejects.toThrow("offline");
  });
});

describe("hotLoadInto 回落语义", () => {
  it("成功 → addResourceBundle 返回 true", async () => {
    const addResourceBundle = vi.fn();
    const fetchImpl = vi.fn().mockResolvedValue(jsonResponse({ a: { b: "x" } }));
    const ok = await hotLoadInto({ addResourceBundle }, "zh-CN", { fetchImpl });
    expect(ok).toBe(true);
    expect(addResourceBundle).toHaveBeenCalledWith(
      "zh-CN",
      "translation",
      { a: { b: "x" } },
      true,
      true,
    );
  });

  it("失败 → 返回 false 不抛（内置 en 兜底）", async () => {
    const addResourceBundle = vi.fn();
    const fetchImpl = vi.fn().mockRejectedValue(new Error("offline"));
    const ok = await hotLoadInto({ addResourceBundle }, "en", { fetchImpl });
    expect(ok).toBe(false);
    expect(addResourceBundle).not.toHaveBeenCalled();
  });
});
