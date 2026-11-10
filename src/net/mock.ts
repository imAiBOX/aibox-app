/**
 * 内存 mock（VITE_AIBOX_MOCK=1）：返回可信假数据，覆盖各状态分支。
 * 场景开关：setMockScenario() 让 UI 单测/开发可切错误路径。
 * 形状 = Rust 实际返回（契约核对后统一）。
 */
import type {
  AppIndex,
  AppManifest,
  ConfigWriteInput,
  ConfigWriteResult,
  DiagReport,
  DiagReportItem,
  DownloadTask,
  ExecTask,
  NetProbeRow,
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
  | "verify_fail" // VERIFY：模型拒绝（401，非网络因）
  | "verify_net" // VERIFY：网络挂（请求发不出）
  | "verify_weak" // VERIFY：弱判定（ok 但无文本 → TS 侧拒绝）
  | "config_write_fail" // C5：备份失败（如无写权限）
  | "update_available"; // 升级黄条

let scenario: MockScenario = "normal";
export function setMockScenario(s: MockScenario): void {
  scenario = s;
}
export function getMockScenario(): MockScenario {
  return scenario;
}

/** 9 项检查（Rust scan.rs check() 同序）；human 即 i18n key */
function checks(report: {
  disk: "ok" | "warn" | "fail";
  port: "ok" | "warn" | "fail";
  node: "ok" | "warn" | "fail";
  git: "ok" | "warn" | "fail";
}): ScanReport["checks"] {
  const row = (id: string, level: "ok" | "warn" | "fail"): ScanReport["checks"][number] => ({
    id,
    level,
    human: `scan.${id}`,
  });
  return [
    row("os", "ok"),
    row("arch", "ok"),
    row("disk", report.disk),
    row("mem", "ok"),
    row("node", report.node),
    row("git", report.git),
    row("wsl", "warn"),
    row("port", report.port),
    row("installed", "ok"),
  ];
}

/* ── 系统体检 ── */
export function mockScanSystem(): ScanReport {
  const portOk = scenario !== "port_occupied";
  const diskOk = scenario !== "disk_low";
  return {
    os: { platform: "windows", version: "Windows 11 Pro 23H2", arch: "x64" },
    diskFreeGb: diskOk ? 128 : 2.1,
    memTotalGb: 16,
    // node 未装/不满足 runtime（可信常态：SCAN 提示需另装）
    node: { installed: false },
    git: { installed: true, version: "2.56.0" },
    wsl: { installed: false },
    installedApps: [],
    port18789: portOk
      ? { occupied: false }
      : { occupied: true, process: "another-assistant.exe" },
    checks: checks({
      disk: diskOk ? "ok" : "fail",
      port: portOk ? "ok" : "fail",
      node: "fail",
      git: "ok",
    }),
  };
}

/* ── 应用目录 ── */
export function mockRegistryIndex(): AppIndex {
  return {
    registryVersion: "1",
    apps: [
      {
        id: "openclaw",
        name: "OpenClaw",
        tagline: {
          "zh-CN": "把大模型接到你的聊天软件：多渠道 AI 网关",
          en: "Multi-channel AI gateway that plugs LLMs into your messengers",
        },
        difficulty: 3,
        platforms: ["windows", "macos"],
        status: "stable",
      },
      {
        id: "hermes",
        name: "Hermes Agent",
        tagline: {
          "zh-CN": "多智能体编排器，配置更重。",
          en: "Multi-agent orchestrator. Heavier setup.",
        },
        difficulty: 4,
        platforms: ["windows"],
        // 非 stable → S1 徽章「版本核对中」
        status: "beta",
      },
      {
        id: "openclaw-old",
        name: "OpenClaw (旧版)",
        tagline: {
          "zh-CN": "上一版本，保留兼容。",
          en: "Previous release, kept for compatibility.",
        },
        difficulty: 1,
        platforms: ["windows", "macos"],
        status: "stable",
        installedVersion: "0.9.2",
      },
    ],
  };
}

const FAKE_SHA = "0".repeat(64);

/** mock app.json（形状 = registry 真文档；routePlans mock-full 步骤 = 4 任务队列） */
export function mockRegistryApp(id: string): AppManifest {
  void id;
  const pkg = (
    p: string,
    name: string,
    version: string,
    installType: string,
    args: string[],
    probe: { cmd: string; expect: string },
    url: string,
    sizeBytes: number,
  ) => ({
    id: p,
    name,
    version,
    platforms: ["windows", "macos"],
    arch: "any",
    sha256: FAKE_SHA,
    sizeBytes,
    channels: [
      { type: "direct", url, region: "global" },
      { type: "mirror", url: `https://npmmirror.com/${url}`, region: "cn" },
    ],
    install: { type: installType, args, probe },
  });

  return {
    id: "openclaw",
    name: "OpenClaw",
    version: "2026.11.2",
    packages: [
      pkg(
        "node",
        "Node.js LTS",
        "v26.13.0",
        "msi-silent",
        ["/qn", "/norestart"],
        { cmd: "node --version", expect: "^v26\\." },
        "https://nodejs.org/dist/v26.13.0/node-v26.13.0-x64.msi",
        36_000_000,
      ),
      pkg(
        "git",
        "Git",
        "2.56.0",
        "nsis-silent",
        ["/VERYSILENT", "/NORESTART"],
        { cmd: "git --version", expect: "^git version 2\\.56" },
        "https://github.com/git-for-windows/git/releases/download/v2.56.0/Git-64-bit.exe",
        67_000_000,
      ),
      pkg(
        "openclaw",
        "OpenClaw",
        "2026.11.2",
        "npm-prefix",
        ["install", "-g", "openclaw@2026.11.2"],
        { cmd: "openclaw --version", expect: "2026\\.11" },
        "https://registry.npmjs.org/openclaw/-/openclaw-2026.11.2.tgz",
        113_000_000,
      ),
      pkg(
        "hub",
        "OpenClaw Windows Hub",
        "v2026.11.1",
        "nsis-silent",
        ["/S"],
        { cmd: "openclaw --version", expect: "2026\\.11" },
        "https://github.com/openclaw/openclaw-windows-node/releases/download/v2026.11.1/Hub-Setup.exe",
        141_000_000,
      ),
    ],
    // mock 路线：mock-full = 全部 4 步（真 registry 为 nsis-hub / npm-prefix 两步路线）
    routes: { global: ["mock-full"], cn: ["mock-full"] },
    routePlans: {
      "mock-full": {
        label: "Mock full queue",
        steps: ["node", "git", "openclaw", "hub"],
      },
    },
    // C3 keyFormat / C4 recommendedModels 真源兑底（与 aibox-registry app.json 同构）
    modelProviders: {
      openrouter: {
        authType: "api-key",
        keyFormat: "^sk-or-v1-[A-Za-z0-9]{64}$",
        configTarget: "~/.config/openclaw/openclaw.json5",
        getKeyUrl: "https://openrouter.ai/keys",
        recommendedModels: [
          { id: "anthropic/claude-sonnet-4", name: "Claude Sonnet 4", note: "聪明均衡，日常首选", priceHint: "中等" },
          { id: "openai/gpt-5", name: "GPT-5", note: "复杂任务更强", priceHint: "偏高" },
          { id: "google/gemini-2.5-flash", name: "Gemini 2.5 Flash", note: "便宜快速", priceHint: "低" },
        ],
      },
      anthropic: {
        authType: "api-key",
        keyFormat: "^sk-ant-[A-Za-z0-9_-]{40,}$",
        configTarget: "~/.config/openclaw/openclaw.json5",
        getKeyUrl: "https://console.anthropic.com/settings/keys",
        recommendedModels: [
          { id: "claude-sonnet-4-5", name: "Claude Sonnet 4.5", note: "性价比均衡", priceHint: "中等" },
          { id: "claude-haiku-4-5", name: "Claude Haiku 4.5", note: "快而便宜", priceHint: "低" },
        ],
      },
      openai: {
        authType: "api-key",
        keyFormat: "^sk-(proj-)?[A-Za-z0-9_-]{20,}$",
        configTarget: "~/.config/openclaw/openclaw.json5",
        getKeyUrl: "https://platform.openai.com/api-keys",
        recommendedModels: [
          { id: "gpt-5.1", name: "GPT-5.1", note: "默认推荐", priceHint: "中等" },
          { id: "gpt-5-mini", name: "GPT-5 mini", note: "省预算", priceHint: "低" },
        ],
      },
    },
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

export function mockNetProbe(): NetProbeRow[] {
  const dead = scenario === "net_dead";
  const row = (resource: string, channel: string, name: string, url: string, ok: boolean, ttfbMs: number, rank: number): NetProbeRow => ({
    resource,
    channel,
    name,
    url,
    ok,
    ttfbMs: ok ? ttfbMs : 0,
    error: ok ? undefined : "connection timed out",
    rank,
  });
  return [
    row("github", "ghfast.top", "GitHub (ghfast proxy)", "https://github.com", !dead, 320, 1),
    row("github", "github.com", "GitHub direct", "https://github.com", !dead, dead ? 0 : 890, 2),
    row("npm", "npmmirror", "npmmirror", "https://registry.npmmirror.com", true, 150, 3),
  ];
}

export function mockNetdoctorRun(): DiagReport {
  const now = Date.now();
  const dead = scenario === "net_dead";
  const row = (
    item: string,
    status: "ok" | "warn" | "fail",
    en: string,
    zh: string,
    fixes: string[] = [],
    detail?: string,
  ): DiagReportItem => ({
    item,
    status,
    summary: { en, "zh-CN": zh },
    detail,
    timestampMs: now,
    fixes,
  });
  return {
    schema: "aibox.diag.v1",
    generatedAtMs: now,
    items: [
      row("D0", "ok", "DNS resolves normally", "DNS 解析正常"),
      row("D1", "ok", "Encrypted DNS works", "加密 DNS 可用"),
      row("D2", "warn", "Proxy detected but not required", "检测到代理但非必需", ["R1"]),
      row("D3", "ok", "Certificates are valid", "证书链有效"),
      row(
        "D4",
        dead ? "fail" : "ok",
        dead ? "No download source reachable" : "Download sources reachable",
        dead ? "下载源全部不可达" : "下载源可达",
        dead ? ["R1", "R2"] : [],
        dead ? "github.com: timeout; ghfast.top: timeout" : undefined,
      ),
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
  const taskId = task?.id ?? "task";
  // 模拟 8MB 分段进度：20 步 × 50ms
  let step = 0;
  const total = 8 * 1024 * 1024;
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
        event: "log_line",
        payload: { text: `download ok: ${total} bytes via ${channel} (${task?.destName ?? "file"})` },
      });
      globalThis.__aiboxEmit?.({
        event: "task_phase",
        payload: { taskId, phase: "verify" },
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
  globalThis.__aiboxEmit?.({
    event: "log_line",
    payload: { text: "download cancelled (partial kept for resume)" },
  });
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
export function mockConfigWrite(input: ConfigWriteInput): ConfigWriteResult {
  // disk_low：磁盘满备份写不进；config_write_fail：无写权限等备份失败
  if (scenario === "disk_low" || scenario === "config_write_fail") {
    return { backupPath: "", ok: false };
  }
  void input;
  return {
    backupPath: "C:\\Users\\dev\\.config\\openclaw\\openclaw.json5.bak.20261109-120000",
    ok: true,
    mergedKeys: ["models", "providers"],
  };
}

export function mockVerifySend(input: VerifyInput): { ok: boolean; text?: string; error?: string } {
  void input;
  if (scenario === "verify_fail") {
    // 模型侧拒绝：key 无效（非弱判定——有明确 error）
    return { ok: false, error: "provider rejected key (401 unauthorized)" };
  }
  if (scenario === "verify_net") {
    // 网络挂：请求根本没发出去
    return { ok: false, error: "network unreachable: connection timed out" };
  }
  if (scenario === "verify_weak") {
    // 弱判定：ok 但无文本 → TS 侧 runVerify 必须拒绝
    return { ok: true };
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
