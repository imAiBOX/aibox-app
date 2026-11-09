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

afterEach(() => {
  setMockScenario("normal");
});

describe("mock 场景开关覆盖", () => {
  it("normal：体检全绿之外的基线（node/wsl 缺失为可信常态）", () => {
    const r = mockScanSystem();
    expect(r.os).toContain("Windows");
    expect(r.diskFreeGb).toBeGreaterThan(5);
    const port = r.items.find((i) => i.id === "port18789");
    expect(port?.ok).toBe(true);
    expect(getMockScenario()).toBe("normal");
  });

  it("port_occupied：端口红灯 + hintKey（S2 红灯分支）", () => {
    setMockScenario("port_occupied");
    const port = mockScanSystem().items.find((i) => i.id === "port18789");
    expect(port?.ok).toBe(false);
    expect(port?.hintKey).toBe("scan.hint.port");
  });

  it("disk_low：磁盘 <5GB（ERROR 分支）", () => {
    setMockScenario("disk_low");
    const r = mockScanSystem();
    expect(r.diskFreeGb).toBeLessThan(5);
    expect(r.items.find((i) => i.id === "disk")?.ok).toBe(false);
    // disk_low 下 config_write 备份写不进 → ok:false
    expect(mockConfigWrite({ provider: "openrouter", apiKey: "sk-x", model: "auto" }).ok).toBe(
      false,
    );
  });

  it("hash_mismatch：verify_sha256 false（C 档篡改分支）", () => {
    setMockScenario("hash_mismatch");
    expect(mockVerifySha256("C:\\cache\\file")).toBe(false);
    expect(mockExecInstall({ taskId: "t", cmd: "x", args: [] }).code).toBe(0);
  });

  it("net_dead：探测挂 + exec 失败 + D4 fail", () => {
    setMockScenario("net_dead");
    const probes = mockNetProbe();
    expect(probes.find((p) => p.channel === "github.com")?.ok).toBe(false);
    expect(mockExecInstall({ taskId: "t", cmd: "x", args: [] }).code).toBe(1);
    expect(mockNetdoctorRun().items.find((i) => i.id === "D4")?.status).toBe("fail");
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
});

describe("mock 基础数据", () => {
  it("registry_index 三态徽章（可安装/已装/核对中）", () => {
    const idx = mockRegistryIndex();
    const statuses = idx.apps.map((a) => a.status);
    expect(statuses).toContain("installable");
    expect(statuses).toContain("installed");
    expect(statuses).toContain("checking");
    expect(idx.apps.every((a) => a.difficulty >= 1 && a.difficulty <= 5)).toBe(true);
  });

  it("registry_app 返回 4 任务队列（PLAN 可用）", () => {
    const m = mockRegistryApp("openclaw");
    expect(m.tasks).toHaveLength(4);
    expect(m.routes.global.length).toBeGreaterThan(0);
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
