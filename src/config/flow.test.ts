/**
 * C1→C2→C3→C4→C5→VERIFY 全流转（mock）：
 * 向导 reducer 驱动 + 真 invoke 入口（VITE_AIBOX_MOCK=1 走内存 mock）。
 * 覆盖：TODO 占位分流、key 校验守卫、C5 写入备份展示、VERIFY_OK 才推进、
 *       弱判定拒绝（ok 无文本）、模型拒绝 / 网络挂的人话错误。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createWizard,
  humanVerifyError,
  reduceWizard,
  runConfigWrite,
  runVerify,
  wizardKeyCheck,
  wizardModels,
  type WizardState,
} from "./flow";
import { reduce, createInitialState, type Event } from "../state";
import { setMockScenario } from "../net/mock";

vi.stubEnv("VITE_AIBOX_MOCK", "1");

afterEach(() => {
  setMockScenario("normal");
});

/** 驱动向导到 C3 粘贴了合法 key 的状态 */
function toC3WithKey(key: string): WizardState {
  let w = createWizard();
  w = reduceWizard(w, { type: "PICK", providerId: "openrouter" }); // C1→C2
  w = reduceWizard(w, { type: "NEXT" }); // C2→C3
  w = reduceWizard(w, { type: "SET_KEY", key });
  return w;
}

const GOOD_KEY = `sk-or-v1-${"a".repeat(64)}`;

describe("C1→C2→C3→C4→C5 流转", () => {
  it("完整走通：选路→陪跑→粘 key→选模型→写入", async () => {
    let w = createWizard();
    expect(w.step).toBe("C1");

    // C1 选 OpenRouter（完整路径）→ C2 陪跑
    w = reduceWizard(w, { type: "PICK", providerId: "openrouter" });
    expect(w.step).toBe("C2");
    expect(w.providerId).toBe("openrouter");

    // C2 → C3 粘贴 key
    w = reduceWizard(w, { type: "NEXT" });
    expect(w.step).toBe("C3");

    // C3 校验守卫：坏 key 不放行
    w = reduceWizard(w, { type: "SET_KEY", key: "oops" });
    expect(wizardKeyCheck(w)?.ok).toBe(false);
    w = reduceWizard(w, { type: "NEXT" });
    expect(w.step).toBe("C3");

    // 好 key → C4
    w = reduceWizard(w, { type: "SET_KEY", key: GOOD_KEY });
    expect(wizardKeyCheck(w)?.ok).toBe(true);
    w = reduceWizard(w, { type: "NEXT" });
    expect(w.step).toBe("C4");

    // C4 模型清单（registry mock 真数据 3 条）+ 选择守卫
    const metas = (await import("../net/mock")).mockRegistryApp("openclaw").modelProviders;
    w = reduceWizard(w, { type: "SET_META", metas: metas! });
    const models = wizardModels(w);
    expect(models.length).toBeGreaterThanOrEqual(3);
    expect(models[0]).toHaveProperty("priceHint");
    w = reduceWizard(w, { type: "NEXT" }); // 未选模型 → 不放行
    expect(w.step).toBe("C4");
    w = reduceWizard(w, { type: "SET_MODEL", model: models[0].id });
    w = reduceWizard(w, { type: "NEXT" });
    expect(w.step).toBe("C5");

    // C5 写入（mock：备份成功）
    w = reduceWizard(w, { type: "WRITE_START" });
    expect(w.writing).toBe(true);
    const out = await runConfigWrite({
      appId: "openclaw",
      provider: "openrouter",
      apiKey: GOOD_KEY,
      model: models[0].id,
    });
    w = reduceWizard(w, { type: "WRITE_DONE", ok: out.ok, backupPath: out.backupPath });
    expect(w.writeResult?.ok).toBe(true);
    expect(w.writeResult?.backupPath).toContain("openclaw.json5.bak.");
    expect(w.errorKey).toBeNull();
  });

  it("C1 选 TODO 路径 → TODO 占位（不装能用），BACK 回 C1", () => {
    let w = createWizard();
    for (const id of ["google", "claude-oauth", "ollama"]) {
      w = reduceWizard(w, { type: "PICK", providerId: id });
      expect(w.step).toBe("TODO");
      w = reduceWizard(w, { type: "BACK" });
      expect(w.step).toBe("C1");
    }
  });

  it("换 provider 重置 key/模型（防串号）", () => {
    let w = toC3WithKey(GOOD_KEY);
    w = reduceWizard(w, { type: "PICK", providerId: "anthropic" });
    expect(w.step).toBe("C2");
    expect(w.key).toBe("");
    expect(w.model).toBeNull();
  });

  it("C5 写入失败 → 人话 errorKey，可回退", () => {
    let w = createWizard();
    w = reduceWizard(w, { type: "PICK", providerId: "openrouter" });
    w = reduceWizard(w, { type: "NEXT", });
    w = reduceWizard(w, { type: "SET_KEY", key: GOOD_KEY });
    w = reduceWizard(w, { type: "NEXT" });
    w = reduceWizard(w, { type: "SET_MODEL", model: "m1" });
    w = reduceWizard(w, { type: "NEXT" });
    w = reduceWizard(w, { type: "WRITE_START" });
    w = reduceWizard(w, { type: "WRITE_FAIL", errorKey: "s5.writeFail" });
    expect(w.writeResult).toBeNull();
    expect(w.errorKey).toBe("s5.writeFail");
    w = reduceWizard(w, { type: "BACK" });
    expect(w.step).toBe("C4");
  });

  it("config_write 备份失败（config_write_fail 场景）→ runConfigWrite 拒绝", async () => {
    setMockScenario("config_write_fail");
    const out = await runConfigWrite({ appId: "openclaw", provider: "openrouter" });
    expect(out.ok).toBe(false);
    expect(out.errorKey).toBe("s5.writeFail");
  });
});

describe("VERIFY 流转与弱判定拒绝", () => {
  it("状态机：CONFIG → CONFIG_WRITTEN → VERIFY → VERIFY_OK → DONE", () => {
    let s = createInitialState();
    // 快进到 CONFIG（转移表前序已有 79 用例覆盖，这里只搭台）
    s = reduce(s, { type: "START_INSTALL" });
    s = reduce(s, {
      type: "SCAN_DONE",
      report: { os: "Windows", arch: "x64", diskFreeGb: 100, memGb: 16, items: [] },
    });
    s = reduce(s, {
      type: "PLAN_READY",
      plan: {
        appId: "openclaw",
        nodes: [],
        tasks: [{ id: "t1", name: "x", version: "1", phase: "download", status: "done" }],
        route: "global",
        channel: "ghfast.top",
        diskMb: 10,
        estMinutes: 1,
      },
    });
    s = reduce(s, { type: "CONFIRM" });
    s = reduce(s, { type: "BEGIN" });
    s = reduce(s, { type: "TASK_DONE", taskId: "t1" });
    s = reduce(s, { type: "ALL_DONE" });
    expect(s.status).toBe("CONFIG");

    s = reduce(s, { type: "CONFIG_WRITTEN", pathId: "openrouter" });
    expect(s.status).toBe("VERIFY");
    expect(s.configPathId).toBe("openrouter");

    // 未 VERIFY_OK 前绝不到 DONE
    const before = reduce(s, { type: "RESTART" } as Event);
    expect(before.status).toBe("VERIFY");

    s = reduce(s, { type: "VERIFY_OK" });
    expect(s.status).toBe("DONE");
    expect(s.verifyOk).toBe(true);
  });

  it("verify_send 成功 → ok + 文本 + 延迟", async () => {
    const r = await runVerify({ provider: "openrouter", apiKey: GOOD_KEY, model: "auto" });
    expect(r.ok).toBe(true);
    expect(r.text).toBe("OK");
    expect(typeof r.latencyMs).toBe("number");
  });

  it("弱判定拒绝：ok 但无文本 → 不算过（errWeak）", async () => {
    setMockScenario("verify_weak");
    const r = await runVerify({ provider: "openrouter", apiKey: GOOD_KEY });
    expect(r.ok).toBe(false);
    expect(r.errorKey).toBe("s6.errWeak");
    expect(r.text).toBeUndefined();
  });

  it("模型拒绝（401）→ errKey 人话；网络挂 → errNet 人话", async () => {
    setMockScenario("verify_fail");
    const rej = await runVerify({ provider: "openrouter", apiKey: "sk-test-bad" });
    expect(rej.ok).toBe(false);
    expect(rej.errorKey).toBe("s6.errKey");
    expect(rej.detail).toContain("401");

    setMockScenario("verify_net");
    const net = await runVerify({ provider: "openrouter", apiKey: GOOD_KEY });
    expect(net.ok).toBe(false);
    expect(net.errorKey).toBe("s6.errNet");
    expect(net.detail).toContain("timed out");
  });

  it("humanVerifyError：未知错误回落 failGeneric", () => {
    expect(humanVerifyError("something odd")).toBe("s6.failGeneric");
    expect(humanVerifyError(undefined)).toBe("s6.failGeneric");
  });
});
