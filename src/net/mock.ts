/**
 * 内存 mock（VITE_AIBOX_MOCK=1）：返回可信假数据，覆盖各状态分支。
 * 场景开关：setMockScenario() 让 UI 单测/开发可切错误路径。
 */
import type {
  AppIndex,
  AppManifest,
  ConfigWriteInput,
  DiagReport,
  DownloadTask,
  ExecTask,
  ProbeResult,
  ProbeSpec,
  ScanReport,
  UpdateInfo,
  VerifyInput,
} from "./client";

export type MockScenario =
  | "normal" // 全绿
  | "port_occupied" // S2 有红灯（端口 18789 被占）
  | "disk_low" // 磁盘不足 → PLAN 前 ERROR
  | "hash_mismatch" // C 档：镜像篡改 → EXECUTE ERROR
  | "net_dead" // 全通道挂 → NET_ALL_CHANNELS_DEAD
  | "verify_fail" // VERIFY 收不到模型文本
  | "update_available"; // 升级黄条

let scenario: MockScenario = "normal";
export function setMockScenario(s: MockScenario): void {
  scenario = s;
}
export function getMockScenario(): MockScenario {
  return scenario;
}

/* ── 系统体检 ── */
export function mockScanSystem(): ScanReport {
  const portOk = scenario !== "port_occupied";
  const diskOk = scenario !== "disk_low";
  return {
    os: "Windows 11 Pro 23H2",
    arch: "x64",
    diskFreeGb: diskOk ? 128 : 2.1,
    memGb: 16,
    items: [
      { id: "os", labelKey: "scan.os", ok: true, value: "Windows 11 23H2" },
      { id: "arch", labelKey: "scan.arch", ok: true, value: "x64" },
      { id: "disk", labelKey: "scan.disk", ok: diskOk, value: diskOk ? "128 GB" : "2.1 GB", hintKey: "scan.hint.disk" },
      { id: "mem", labelKey: "scan.mem", ok: true, value: "16 GB" },
      { id: "node", labelKey: "scan.node", ok: false, value: "—", hintKey: "scan.hint.node" },
      { id: "git", labelKey: "scan.git", ok: true, value: "2.56.0" },
      { id: "wsl", labelKey: "scan.wsl", ok: false, value: "—", hintKey: "scan.hint.wsl" },      {
        id: "port18789",
        labelKey: "scan.port",
        ok: portOk,
        value: portOk ? "free" : "127.0.0.1:18789",
        hintKey: "scan.hint.port",
      },
      { id: "installed", labelKey: "scan.installed", ok: true, value: "—" },
    ],
  };
}

/* ── 应用目录 ── */
export function mockRegistryIndex(): AppIndex {
  return {
    apps: [
      {
        id: "openclaw",
        name: "OpenClaw",
        summaryKey: "app.openclaw.summary",
        difficulty: 2,
        platforms: ["win", "mac"],
        status: "installable",
      },
      {
        id: "hermes",
        name: "Hermes Agent",
        summaryKey: "app.hermes.summary",
        difficulty: 3,
        platforms: ["win"],
        status: "checking",
      },
      {
        id: "openclaw-old",
        name: "OpenClaw (旧版)",
        summaryKey: "app.openclawOld.summary",
        difficulty: 1,
        platforms: ["win", "mac"],
        status: "installed",
        installedVersion: "0.9.2",
      },
    ],
  };
}

export function mockRegistryApp(id: string): AppManifest {
  void id;
  return {
    id: "openclaw",
    name: "OpenClaw",
    routes: { global: ["ghfast.top", "github.com"], cn: ["npmmirror", "ghproxy"] },
    tasks: [
      { id: "node", name: "Node.js LTS", version: "v26.13.0" },
      { id: "git", name: "Git", version: "2.56.0" },
      { id: "openclaw", name: "OpenClaw", version: "2026.11.2" },
      { id: "hub", name: "OpenClaw Windows Hub", version: "v2026.11.1" },
    ],
  };
}

/* ── 网络 ── */
let region: { source: string; effective: "cn" | "global" } = {
  source: "auto",
  effective: "cn",
};
export function mockNetRegion() {
  return region;
}
export function mockNetRegionSet(v: string) {
  region = { source: v, effective: v === "global" ? "global" : "cn" };
}

export function mockNetProbe(): ProbeResult[] {
  return [
    { channel: "ghfast.top", ok: true, ttfbMs: 320 },
    { channel: "github.com", ok: scenario !== "net_dead", ttfbMs: scenario === "net_dead" ? 0 : 890 },
    { channel: "npmmirror", ok: true, ttfbMs: 150 },
  ];
}

export function mockNetdoctorRun(): DiagReport {
  return {
    items: [
      { id: "D0", name: "DNS 解析", status: "ok", conclusionKey: "diag.D0.ok" },
      { id: "D1", name: "DoH 可用", status: "ok", conclusionKey: "diag.D1.ok" },
      { id: "D2", name: "代理检测", status: "warn", conclusionKey: "diag.D2.warn" },
      { id: "D3", name: "证书链", status: "ok", conclusionKey: "diag.D3.ok" },
      { id: "D4", name: "通道测速", status: scenario === "net_dead" ? "fail" : "ok", conclusionKey: "diag.D4.ok" },
    ],
  };
}

/* ── 执行任务 ── */
let jobCounter = 0;
const activeJobs = new Set<string>();
const progressTimers = new Map<string, ReturnType<typeof setInterval>>();

export function mockDownloadStart(task: DownloadTask): string {
  const jobId = `job-${++jobCounter}`;
  activeJobs.add(jobId);
  // 模拟 8MB 分段进度：20 步 × 50ms
  let step = 0;
  const total = task?.name?.length ? 8 * 1024 * 1024 : 8 * 1024 * 1024;
  const timer = setInterval(() => {
    if (!activeJobs.has(jobId)) {
      clearInterval(timer);
      progressTimers.delete(jobId);
      return;
    }
    step += 1;
    const channel = scenario === "net_dead" ? "github.com" : "ghfast.top";
    globalThis.__aiboxEmit?.({
      event: "download_progress",
      payload: { jobId, done: Math.min(step * 640_000, total), total, channel },
    });
    if (step >= 20) {
      clearInterval(timer);
      progressTimers.delete(jobId);
      activeJobs.delete(jobId);
      globalThis.__aiboxEmit?.({
        event: "task_phase",
        payload: { taskId: task?.taskId ?? "task", phase: "verify" },
      });
    }
  }, 50);
  progressTimers.set(jobId, timer);
  return jobId;
}

export function mockDownloadCancel(jobId: string): void {
  activeJobs.delete(jobId);
  const t = progressTimers.get(jobId);
  if (t) clearInterval(t);
  progressTimers.delete(jobId);
}

export function mockVerifySha256(path: string): boolean {
  void path;
  return scenario !== "hash_mismatch";
}

export function mockExecInstall(task: ExecTask): { code: number; out: string } {
  void task;
  if (scenario === "net_dead") return { code: 1, out: "download failed: all channels dead" };
  return { code: 0, out: "installed ok" };
}

export function mockProbeRun(spec: ProbeSpec): { version: string } {
  void spec;
  return { version: "2026.11.2" };
}

/* ── CONFIG / VERIFY ── */
export function mockConfigWrite(input: ConfigWriteInput): { backupPath: string; ok: boolean } {
  void input;
  if (scenario === "disk_low") return { backupPath: "", ok: false };
  return { backupPath: "C:\\Users\\dev\\.config\\openclaw\\openclaw.json5.bak.20261109", ok: true };
}

export function mockVerifySend(input: VerifyInput): { ok: boolean; text?: string; error?: string } {
  void input;
  if (scenario === "verify_fail") {
    return { ok: false, error: "provider rejected key (401)" };
  }
  return { ok: true, text: "OK", error: undefined };
}

/* ── 诊断 / 更新 / locales ── */
export function mockDiagExport(): string {
  return "C:\\Users\\dev\\Desktop\\aibox-diagnostic-20261109.zip";
}

export function mockUpdateCheck(): UpdateInfo {
  if (scenario === "update_available" || scenario === "normal") {
    // normal 下也给一次可用更新，方便看黄条；测试可用场景开关控制
    return scenario === "update_available"
      ? { hasUpdate: true, version: "0.1.0", url: "https://aibox.im/download" }
      : { hasUpdate: false };
  }
  return { hasUpdate: false };
}

const localesCache: Record<string, Record<string, string>> = {
  en: { "mock.remote.key": "Remote loaded value" },
  "zh-CN": { "mock.remote.key": "远程加载的值" },
};
export function mockLocalesLoad(lang: string): Record<string, string> {
  // 模拟 registry locales 热加载；未知语言抛错 → loader 回落内置 en
  const hit = localesCache[lang];
  if (!hit) throw new Error(`locales not found: ${lang}`);
  return hit;
}

/** 测试直接推进进度的辅助（client.testHooks 暴露） */
export function mockEvents() {
  return activeJobs;
}
export function mockDownloadProgress(jobId: string, done: number, total: number, channel: string) {
  globalThis.__aiboxEmit?.({
    event: "download_progress",
    payload: { jobId, done, total, channel },
  });
}
