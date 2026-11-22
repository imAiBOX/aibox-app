import { describe, expect, it } from "vitest";
import { buildCandidates, keyOk, summarize, writableOf, writableSelected } from "./batch";

describe("批量「一个 key」：可写性判定", () => {
  it("registry 明确 true → 可写", () => {
    expect(writableOf({ configWritable: true })).toEqual({ writable: true });
  });

  it("registry 明确 false → 不可写（不是未知）", () => {
    expect(writableOf({ configWritable: false })).toEqual({
      writable: false,
      reason: "notSupported",
    });
  });

  it("元数据缺席 → 未知（也不写）", () => {
    expect(writableOf(undefined)).toEqual({ writable: false, reason: "unknown" });
  });
});

describe("候选列表", () => {
  const installed = [{ appId: "openclaw" }, { appId: "chatbox" }, { appId: "zed" }];
  const metas = {
    openclaw: { configWritable: true },
    chatbox: { configWritable: false },
    zed: undefined,
  };

  it("可写的排前面，且带出不可写原因", () => {
    const list = buildCandidates(installed, metas);
    expect(list[0].appId).toBe("openclaw");
    expect(list[1].appId).toBe("chatbox");
    expect(list[1].writable).toBe(false);
    expect(list[1].reason).toBe("notSupported");
    expect(list[2].reason).toBe("unknown");
  });

  it("默认不勾选 → 计划为空（批量写不是默认动作）", () => {
    const list = buildCandidates(installed, metas);
    expect(writableSelected(list, [])).toHaveLength(0);
  });

  it("勾了不可写的 → 不写（不假装写）", () => {
    const list = buildCandidates(installed, metas);
    const plan = writableSelected(list, ["chatbox", "zed"]);
    expect(plan).toHaveLength(0);
  });

  it("勾了可写的 → 进计划", () => {
    const list = buildCandidates(installed, metas);
    expect(writableSelected(list, ["openclaw"]).map((c) => c.appId)).toEqual(["openclaw"]);
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
