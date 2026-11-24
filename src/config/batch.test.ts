import { profiles } from "./config-capability-v1.fixtures.json";
import { describe, expect, it } from "vitest";
import { buildCandidates, keyOk, summarize, writableSelected } from "./batch";

describe("候选列表", () => {
  const installed = [{ appId: "aider" }, { appId: "unlisted" }, { appId: "zed" }];
  const candidates = () => buildCandidates(installed, profiles, "openai", "macos");

  it("可写的排前面，且带出不可写原因", () => {
    const list = candidates();
    expect(list[0].appId).toBe("aider");
    expect(list[1].reason).toBe("unknown");
    expect(list[2].appId).toBe("zed");
    expect(list[2].writable).toBe(false);
    expect(list[2].reason).toBe("notSupported");
  });

  it("默认不勾选 → 计划为空（批量写不是默认动作）", () => {
    const list = candidates();
    expect(writableSelected(list, [])).toHaveLength(0);
  });

  it("勾了不可写的 → 不写（不假装写）", () => {
    const list = candidates();
    const plan = writableSelected(list, ["unlisted", "zed"]);
    expect(plan).toHaveLength(0);
  });

  it("勾了可写的 → 进计划", () => {
    const list = candidates();
    expect(writableSelected(list, ["aider"]).map((c) => c.appId)).toEqual(["aider"]);
  });

  it("Windows 有目录条目也只支持手动配置", () => {
    const list = buildCandidates(installed, profiles, "openai", "windows");
    expect(list.find((c) => c.appId === "aider")).toMatchObject({ writable: false, reason: "notSupported" });
    expect(writableSelected(list, ["aider", "zed"])).toHaveLength(0);
  });
});

describe("回执汇总", () => {
  it("一个失败不影响其他（成功/失败分别计数）", () => {
    const r = summarize([
      { appId: "a", name: "a", ok: true },
      { appId: "b", name: "b", ok: false, errorCode: "CONFIG_NO_TARGET" },
      { appId: "c", name: "c", ok: true },
    ]);
    expect(r).toEqual({ ok: 2, failed: 1, total: 3 });
  });
});

describe("key 格式校验", () => {
  it("空 key 不通过", () => {
    expect(keyOk("   ")).toBe(false);
  });

  it("无格式要求 → 非空即通过", () => {
    expect(keyOk("abc")).toBe(true);
  });

  it("按 registry 正则校验", () => {
    expect(keyOk("sk-or-v1-abcdefgh", "^sk-or-v1-[A-Za-z0-9]+$")).toBe(true);
    expect(keyOk("nope", "^sk-or-v1-[A-Za-z0-9]+$")).toBe(false);
  });

  it("正则坏掉 → 不拦用户（后端仍会校验）", () => {
    expect(keyOk("whatever", "([unclosed")).toBe(true);
  });
});
