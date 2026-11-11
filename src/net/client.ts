/**
 * TS↔Rust invoke 单一入口（签名冻结）。
 * `VITE_AIBOX_MOCK=1` 时全部走内存 mock（UI 开发与单测不依赖 Rust）。
 * 事件订阅：Rust 侧 emit（download_progress / task_phase / channel_switched / log_line），
 * mock 模式下由 mockEvents 模拟推送。
 *
 * 本文件类型以 src-tauri/commands.rs 实际签名为准。

 */
import {
  mockDownloadProgress,
  mockEvents,
  mockExecInstall,
  mockLocalesLoad,
  mockNetProbe,
  mockNetRegion,
  mockNetRegionSet,
  mockNetdoctorRun,
  mockProbeRun,
  mockRegistryApp,
  mockRegistryIndex,
  mockScanSystem,
  mockUpdateCheck,
  mockVerifySend,
  mockVerifySha256,
  mockDiagExport,
  mockConfigWrite,
  mockDownloadStart,
  mockDownloadCancel,
  getMockScenario,
} from "./mock";
import type { ProviderId, ProviderMeta } from "../config/types";

export type { ProviderId, ProviderMeta };
/** mock 场景开关读取（App 的 EXECUTE 失败注入用；真分支不读） */
export { getMockScenario };

/** `net_region()` 返回的三锚点实测摘要（auto 判定时给出） */
export interface RegionAnchors {
  github: { ok: boolean; ttfbMs: number };
  releaseAsset: { ok: boolean; ttfbMs: number };
  npmmirror: { ok: boolean; ttfbMs: number };
}
/** `net_region()` 返回（source + 生效区域 + 可选三锚点实测） */
export interface NetRegionResult {
  source: string;
  effective: "cn" | "global";
  anchors?: RegionAnchors;
}

export const isMock = (): boolean => {
  const metaEnv =
    typeof import.meta !== "undefined"
      ? (import.meta as { env?: Record<string, string> }).env
      : undefined;
  if (metaEnv?.VITE_AIBOX_MOCK === "1") return true;
  // 测试环境（vitest vi.stubEnv 只写 process.env）与 node 侧兑底
  const proc = (globalThis as { process?: { env?: Record<string, string> } }).process;
  return proc?.env?.VITE_AIBOX_MOCK === "1";
};

/* ── 类型（= Rust 实际返回/payload；扫描报告形状） ── */

/** 单条体检结论（Rust `Check`；`human` 即 i18n key `scan.<id>`） */
export interface ScanCheck {
  id: string;
  level: "ok" | "warn" | "fail";
  human: string;
}
/** `scan_system` 返回（Rust aibox_core::scan::ScanReport，camelCase） */
export interface ScanReport {
  os: { platform: string; version: string; arch: string };
  diskFreeGb: number;
  memTotalGb: number;
  node: { installed: boolean; version?: string; ok?: boolean; note?: string };
  git: { installed: boolean; version?: string };
  wsl: { installed: boolean; distros?: string[] };
  installedApps: { id: string; version?: string; path?: string }[];
  port18789: { occupied: boolean; process?: string };
  checks: ScanCheck[];
}

/** registry index.json 单个应用（+ Rust enrich 的 installedVersion） */
export interface AppIndexEntry {
  id: string;
  name: string;
  category?: string;
  /** 双语一句话简介（registry 数据，非 i18n key） */
  tagline?: Record<string, string>;
  icon?: string;
  difficulty?: number;
  popularity?: number;
  platforms: string[];
  /** registry 状态原值（如 "stable"）；徽章由 appBadge() 派生 */
  status: string;
  installedVersion?: string;
}
/** `registry_index` 返回（裸 index.json 文档 + installedVersion 注入） */
export interface AppIndex {
  registryVersion?: string;
  generatedAt?: string;
  note?: string;
  apps: AppIndexEntry[];
}

/** registry 下载通道（type 大小写不敏感：direct/mirror/proxy/custom） */
export interface RegistryChannel {
  type: string;
  url: string;
  region?: string;
  operator?: string;
  note?: string;
}
/** registry 安装描述 */
export interface RegistryInstall {
  type: string;
  args?: string[];
  probe?: { cmd: string; expect: string };
  notes?: string;
}
/** registry 单个安装包 */
export interface RegistryPackage {
  id: string;
  name: string;
  version: string;
  platforms: string[];
  arch?: string; // 'x64' | 'arm64' | 'any' | 缺省
  sha256?: string;
  sizeBytes?: number;
  channels: RegistryChannel[];
  install?: RegistryInstall;
}
export interface RoutePlan {
  label?: string;
  steps: string[];
  note?: string;
}
/** `registry_app` 返回（裸 app.json 文档） */
export interface AppManifest {
  id: string;
  name: string;
  version?: string;
  packages: RegistryPackage[];
  routes?: Record<string, string[]>;
  routePlans?: Record<string, RoutePlan>;
  /** registry modelProviders（C3 keyFormat / C4 recommendedModels 真源；可缺省→本地回落） */
  modelProviders?: Record<string, ProviderMeta>;
}

/** DownloadTask（download_start payload；appId/sizeBytes/skipChannels 为可选扩展字段） */
export interface DownloadTask {
  id: string;
  channels: { type: string; url: string }[];
  sha256: string;
  destName: string;
  /** 所属应用 id（任务日志子目录 ~/.aibox/logs/<app>/） */
  appId?: string;
  /** registry 包级期望字节数（通道无关；截断检测用） */
  sizeBytes?: number;
  /** 换源重发：跳过的通道类型（如 ["PROXY"]） */
  skipChannels?: string[];
}
/** ExecTask.kind（Rust ExecKind::parse 白名单） */
export type ExecKind = "nsis-silent" | "npm-prefix" | "script";
/** ExecTask（exec_install payload；appId 为可选扩展字段，任务日志用） */
export interface ExecTask {
  id: string;
  kind: ExecKind;
  exe: string;
  args: string[];
  cwd?: string;
  probe?: ProbeSpec;
  appId?: string;
}
/** ProbeSpec（probe_run payload） */
export interface ProbeSpec {
  cmd: string;
  expect: string;
}
/** net_probe 行（Rust NetProbeRow，camelCase） */
export interface NetProbeRow {
  resource: string;
  channel: string;
  name: string;
  url: string;
  ok: boolean;
  ttfbMs: number;
  status?: number;
  error?: string;
  rank: number;
}
export interface DiagReportItem {
  /** 'D0'..'D4'（冻结） */
  item: string;
  status: "ok" | "warn" | "fail";
  summary: { en: string; "zh-CN": string };
  detail?: string;
  timestampMs: number;
  fixes?: string[]; // 'R1'|'R2'|'R3'
}
export interface DiagReport {
  schema?: string;
  generatedAtMs?: number;
  items: DiagReportItem[];
}
export interface VerifyInput {
  /** 冻结：provider 联合 + 可选字段 */
  provider: ProviderId;
  apiKey?: string;
  model?: string;
  baseUrl?: string;
}
export interface ConfigWriteInput {
  /** appId 必填，其余可选（缺省用 registry configTarget 展开） */
  appId: string;
  provider: ProviderId;
  apiKey?: string;
  baseUrl?: string;
  model?: string;
  targetPath?: string;
}
export interface ConfigWriteResult {
  backupPath: string;
  ok: boolean;
  mergedKeys?: string[];
}
export interface UpdateInfo {
  hasUpdate: boolean;
  version?: string;
  url?: string;
}

/* ── 事件总线 ── */
export type AiboxEvent =
  | { event: "download_progress"; payload: { jobId: string; done: number; total: number; channel: string } }
  | { event: "task_phase"; payload: { taskId: string; phase: string } }
  | { event: "channel_switched"; payload: { from: string; to: string; reason: string } }
  | { event: "log_line"; payload: { text: string } };

type Handler = (e: AiboxEvent) => void;
const handlers = new Set<Handler>();

/** 订阅事件（返回退订函数） */
export function onEvent(h: Handler): () => void {
  handlers.add(h);
  return () => handlers.delete(h);
}

/** mock/测试推入事件 */
export function emitEvent(e: AiboxEvent): void {
  for (const h of [...handlers]) h(e);
}

// mock 定时器/测试辅助经此推事件（避免 mock.ts ↔ client.ts 循环导入）
globalThis.__aiboxEmit = emitEvent;

declare global {
  var __aiboxEmit: ((e: AiboxEvent) => void) | undefined;
}

async function invoke<T>(cmd: string, args?: unknown): Promise<T> {
  if (isMock()) return mockDispatch<T>(cmd, args);
  const { invoke: tauriInvoke } = await import("@tauri-apps/api/core");
  return tauriInvoke<T>(cmd, args as Record<string, unknown> | undefined);
}

function mockDispatch<T>(cmd: string, a?: unknown): T {
  const args = (a ?? {}) as Record<string, unknown>;
  switch (cmd) {
    case "scan_system":
      return mockScanSystem() as T;
    case "registry_index":
      return mockRegistryIndex() as T;
    case "registry_app":
      return mockRegistryApp(String(args.id ?? "openclaw")) as T;
    case "net_region":
      return mockNetRegion() as T;
    case "net_region_set":
      return mockNetRegionSet(String(args?.v ?? "auto")) as T;
    case "net_probe":
      return mockNetProbe() as T;
    case "netdoctor_run":
      return mockNetdoctorRun() as T;
    case "download_start":
      return mockDownloadStart(a as DownloadTask) as T;
    case "download_cancel":
      return mockDownloadCancel(String(args.jobId ?? "")) as T;
    case "verify_sha256":
      return mockVerifySha256(String(args.path ?? "")) as T;
    case "exec_install":
      return mockExecInstall(a as ExecTask) as T;
    case "probe_run":
      return mockProbeRun(a as ProbeSpec) as T;
    case "config_write":
      return mockConfigWrite(a as ConfigWriteInput) as T;
    case "verify_send":
      return mockVerifySend(a as VerifyInput) as T;
    case "diag_export":
      return mockDiagExport() as T;
    case "save_plan":
      return "~/.aibox/cache/last-plan.json" as T;
    case "update_check":
      return mockUpdateCheck() as T;
    case "locales_load":
      return mockLocalesLoad(String(args?.lang ?? "en")) as T;
    default:
      throw new Error(`unknown command: ${cmd}`);
  }
}

/* ── 对外 API ── */
export const api = {
  scanSystem: () => invoke<ScanReport>("scan_system"),
  registryIndex: (force?: boolean) => invoke<AppIndex>("registry_index", { force }),
  registryApp: (id: string) => invoke<AppManifest>("registry_app", { id }),
  netRegion: () => invoke<NetRegionResult>("net_region"),
  netRegionSet: (v: "auto" | "cn" | "global") => invoke<void>("net_region_set", { v }),
  netProbe: () => invoke<NetProbeRow[]>("net_probe"),
  netdoctorRun: (items?: string[]) => invoke<DiagReport>("netdoctor_run", { items }),
  downloadStart: (task: DownloadTask) => invoke<string>("download_start", task),
  downloadCancel: (jobId: string) => invoke<void>("download_cancel", { jobId }),
  verifySha256: (path: string, expected: string) =>
    invoke<boolean>("verify_sha256", { path, expected }),
  execInstall: (task: ExecTask) => invoke<{ code: number; out: string }>("exec_install", task),
  probeRun: (spec: ProbeSpec) => invoke<{ version: string }>("probe_run", spec),
  configWrite: (input: ConfigWriteInput) =>
    invoke<ConfigWriteResult>("config_write", input),
  verifySend: (input: VerifyInput) =>
    invoke<{ ok: boolean; text?: string; error?: string }>("verify_send", input),
  diagExport: () => invoke<string>("diag_export"),
  /** 写入最后计划到 ~/.aibox/cache/last-plan.json（诊断包收录） */
  savePlan: (plan: unknown) => invoke<string>("save_plan", { plan }),
  updateCheck: () => invoke<UpdateInfo>("update_check"),
  localesLoad: (lang: string) => invoke<Record<string, string>>("locales_load", { lang }),
};

/** mock 模式下让测试/开发手动推进下载进度 */
export const testHooks = { mockDownloadProgress, mockEvents };
