/**
 * TS↔Rust invoke 单一入口（签名冻结）。
 * `VITE_AIBOX_MOCK=1` 时全部走内存 mock（UI 开发与单测不依赖 Rust）。
 * 事件订阅：Rust 侧 emit（download_progress / task_phase / channel_switched / log_line），
 * mock 模式下由 mockEvents 模拟推送。
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
} from "./mock";

export const isMock = (): boolean =>
  typeof import.meta !== "undefined" &&
  (import.meta as { env?: Record<string, string> }).env?.VITE_AIBOX_MOCK === "1";

/* ── 类型（与一致） ── */
export interface ScanReport {
  os: string;
  arch: string;
  diskFreeGb: number;
  memGb: number;
  items: { id: string; labelKey: string; ok: boolean; hintKey?: string; value?: string }[];
}
export interface AppIndex {
  apps: {
    id: string;
    name: string;
    summaryKey: string;
    difficulty: number;
    platforms: string[];
    status: "installable" | "installed" | "checking";
    installedVersion?: string;
  }[];
}
export interface AppManifest {
  id: string;
  name: string;
  tasks: { id: string; name: string; version: string }[];
  routes: Record<string, string[]>;
}
export interface DownloadTask {
  taskId: string;
  name: string;
  url: string;
  sha256: string;
}
export interface ExecTask {
  taskId: string;
  cmd: string;
  args: string[];
}
export interface ProbeSpec {
  taskId: string;
  cmd: string;
  args: string[];
}
export interface DiagReport {
  items: { id: string; name: string; status: "ok" | "warn" | "fail"; conclusionKey: string }[];
}
export interface ProbeResult {
  channel: string;
  ok: boolean;
  ttfbMs: number;
}
export interface VerifyInput {
  provider: string;
  apiKey: string;
  model: string;
}
export interface ConfigWriteInput {
  provider: string;
  apiKey: string;
  model: string;
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
  netRegion: () => invoke<{ source: string; effective: "cn" | "global" }>("net_region"),
  netRegionSet: (v: "auto" | "cn" | "global") => invoke<void>("net_region_set", { v }),
  netProbe: () => invoke<ProbeResult[]>("net_probe"),
  netdoctorRun: (items?: string[]) => invoke<DiagReport>("netdoctor_run", { items }),
  downloadStart: (task: DownloadTask) => invoke<string>("download_start", task),
  downloadCancel: (jobId: string) => invoke<void>("download_cancel", { jobId }),
  verifySha256: (path: string, expected: string) =>
    invoke<boolean>("verify_sha256", { path, expected }),
  execInstall: (task: ExecTask) => invoke<{ code: number; out: string }>("exec_install", task),
  probeRun: (spec: ProbeSpec) => invoke<{ version: string }>("probe_run", spec),
  configWrite: (input: ConfigWriteInput) =>
    invoke<{ backupPath: string; ok: boolean }>("config_write", input),
  verifySend: (input: VerifyInput) =>
    invoke<{ ok: boolean; text?: string; error?: string }>("verify_send", input),
  diagExport: () => invoke<string>("diag_export"),
  updateCheck: () => invoke<UpdateInfo>("update_check"),
  localesLoad: (lang: string) => invoke<Record<string, string>>("locales_load", { lang }),
};

/** mock 模式下让测试/开发手动推进下载进度 */
export const testHooks = { mockDownloadProgress, mockEvents };
