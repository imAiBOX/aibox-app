/**
 * mock client 分支覆盖（VITE_AIBOX_MOCK 场景开关 → 各状态分支的可信假数据）。
 * import.meta.env 在 vitest 下可用（vite 注入），这里直接测 mock 层 + 事件总线。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  emitEvent,
  onEvent,
  type AiboxEvent,
} from "./client";
import {
  getMockScenario,
  mockConfigWrite,
  mockExecInstall,
  mockLocalesLoad,
  mockEvents,
  mockNetdoctorRun,
  mockNetProbe,
  mockRegistryApp,
  mockRegistryIndex,
  mockScanSystem,
  mockUpdateCheck,
  mockVerifySend,
  mockVerifySha256,
  setMockScenario,
} from "./mock";
import { scanRows } from "../state/selectors";
import { appBadge } from "../screens/S1Idle";

afterEach(() => {
  setMockScenario("normal");
});

describe("mock 场景开关覆盖", () => {
  it("normal：体检全绿之外的基线（node/wsl 缺失为可信常态）", () => {
    const r = mockScanSystem();
    expect(r.os.version).toContain("Windows");
    expect(r.diskFreeGb).toBeGreaterThan(5);
    const port = r.checks.find((i) => i.id === "port");
    expect(port?.level).toBe("ok");
    expect(port?.human).toBe("scan.port");
    expect(getMockScenario()).toBe("normal");
  });

  it("port_occupied：端口红灯 + hint 由 selectors 派生（S2 红灯分支）", () => {
    setMockScenario("port_occupied");
    const r = mockScanSystem();
    expect(r.checks.find((i) => i.id === "port")?.level).toBe("fail");
    expect(r.port18789.occupied).toBe(true);
    expect(r.port18789.process).toBeTruthy();
    // scanRows 派生 hintKey
    expect(scanRows(r).find((x) => x.id === "port")?.hintKey).toBe("scan.hint.port");
  });

  it("disk_low：磁盘 <5GB（ERROR 分支）", () => {
    setMockScenario("disk_low");
    const r = mockScanSystem();
    expect(r.diskFreeGb).toBeLessThan(5);
    expect(r.checks.find((i) => i.id === "disk")?.level).toBe("fail");
    // disk_low 下 config_write 备份写不进 → ok:false
    expect(
      mockConfigWrite({ appId: "openclaw", provider: "openrouter", apiKey: "sk-x", model: "auto" })
        .ok,
    ).toBe(
      false,
    );
  });

  it("hash_mismatch：verify_sha256 false（C 档篡改分支）", () => {
    setMockScenario("hash_mismatch");
    expect(mockVerifySha256("C:\\cache\\file")).toBe(false);
    expect(mockExecInstall({ id: "t", kind: "script", exe: "x", args: [] }).code).toBe(0);
  });

  it("net_dead：探测挂 + exec 失败 + D4 fail", () => {
    setMockScenario("net_dead");
    const probes = mockNetProbe();
    expect(probes.find((p) => p.channel === "github.com")?.ok).toBe(false);
    expect(mockExecInstall({ id: "t", kind: "script", exe: "x", args: [] }).code).toBe(1);
    expect(mockNetdoctorRun().items.find((i) => i.item === "D4")?.status).toBe("fail");
  });

  it("verify_fail：VERIFY 收不到模型文本", () => {
    setMockScenario("verify_fail");
    const r = mockVerifySend({ provider: "openrouter", apiKey: "sk-x", model: "auto" });
    expect(r.ok).toBe(false);
    expect(r.error).toBeTruthy();
    setMockScenario("normal");
    expect(mockVerifySend({ provider: "openrouter", apiKey: "sk-x", model: "auto" })).toEqual({
      ok: true,
      text: "OK",
      error: undefined,
    });
  });

  it("update_available：升级黄条分支", () => {
    setMockScenario("update_available");
    const u = mockUpdateCheck();
    expect(u.hasUpdate).toBe(true);
    expect(u.url).toBe("https://aibox.im/download");
    setMockScenario("normal");
    expect(mockUpdateCheck().hasUpdate).toBe(false);
  });

  it("config_write：备份成功（normal）/ 失败（config_write_fail）", () => {
    const ok = mockConfigWrite({ appId: "openclaw", provider: "openrouter" });
    expect(ok.ok).toBe(true);
    expect(ok.backupPath).toContain(".bak.");
    expect(ok.mergedKeys?.length).toBeGreaterThan(0);
    setMockScenario("config_write_fail");
    const fail = mockConfigWrite({ appId: "openclaw", provider: "openrouter" });
    expect(fail.ok).toBe(false);
    expect(fail.backupPath).toBe("");
  });

  it("verify_send：成功 / 模型拒绝（401）/ 网络挂 / 弱判定（无文本）", () => {
    const input = { provider: "openrouter", apiKey: "sk-test-000" } as const;
    // 成功
    expect(mockVerifySend(input)).toEqual({ ok: true, text: "OK", error: undefined });
    // 模型拒绝
    setMockScenario("verify_fail");
    expect(mockVerifySend(input).error).toContain("401");
    // 网络挂
    setMockScenario("verify_net");
    expect(mockVerifySend(input).error).toContain("timed out");
    // 弱判定：ok 但无文本 → TS 侧拒绝
    setMockScenario("verify_weak");
    const weak = mockVerifySend(input);
    expect(weak.ok).toBe(true);
    expect(weak.text).toBeUndefined();
  });
});

describe("mock 基础数据", () => {
  it("registry_index 三态徽章（可安装/已装/核对中，appBadge 派生）", () => {
    const idx = mockRegistryIndex();
    const badges = idx.apps.map((a) => appBadge(a));
    expect(badges).toContain("installable");
    expect(badges).toContain("installed");
    expect(badges).toContain("checking");
    expect(idx.apps.every((a) => (a.difficulty ?? 0) >= 1 && (a.difficulty ?? 0) <= 5)).toBe(true);
    // tagline 双语存在（S1 一句话简介）
    expect(idx.apps[0].tagline?.["zh-CN"]).toBeTruthy();
    expect(idx.apps[0].tagline?.en).toBeTruthy();
  });

  it("registry_app 返回 packages 队列 + routePlans（PLAN 可用）+ modelProviders（C3/C4 真源）", () => {
    const m = mockRegistryApp("openclaw");
    expect(m.packages).toHaveLength(4);
    expect(m.routes?.global?.length).toBeGreaterThan(0);
    expect(m.routePlans?.["mock-full"]?.steps).toHaveLength(4);
    // C3 keyFormat + C4 recommendedModels（registry 数据 mock 兑底）
    const or = m.modelProviders?.openrouter;
    expect(or?.keyFormat).toBe("^sk-or-v1-[A-Za-z0-9]{64}$");
    expect(or?.recommendedModels).toHaveLength(3);
    expect(or?.recommendedModels?.[0]).toHaveProperty("priceHint");
    expect(m.modelProviders?.anthropic?.recommendedModels?.length).toBeGreaterThan(0);
    expect(m.modelProviders?.openai?.recommendedModels?.length).toBeGreaterThan(0);
  });

  it("locales_load：未知语言抛错（loader 回落 en）", () => {
    expect(() => mockLocalesLoad("fr")).toThrow("locales not found");
    expect(mockLocalesLoad("zh-CN")).toHaveProperty("mock.remote.key");
  });

  it("diag_export 返回 zip 路径", () => {
    // 直接测 mock 层（client.diagExport 需要 VITE_AIBOX_MOCK，交给集成）
    expect(typeof mockNetdoctorRun().items.length).toBe("number");
  });
});

describe("事件总线", () => {
  it("onEvent 订阅 / 退订", () => {
    const seen: AiboxEvent[] = [];
    const off = onEvent((e) => seen.push(e));
    emitEvent({ event: "log_line", payload: { text: "hello" } });
    off();
    emitEvent({ event: "log_line", payload: { text: "after" } });
    expect(seen).toHaveLength(1);
    expect(seen[0]).toEqual({ event: "log_line", payload: { text: "hello" } });
  });

  it("多订阅者都收到", () => {
    const a = vi.fn();
    const b = vi.fn();
    const offA = onEvent(a);
    const offB = onEvent(b);
    emitEvent({ event: "task_phase", payload: { taskId: "t", phase: "verify" } });
    offA();
    offB();
    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);
  });

  it("mockEvents 暴露活跃 job 集合", () => {
    expect(mockEvents()).toBeInstanceOf(Set);
  });
});
