/**
 * EXECUTE 四阶段编排。
 *
 * - deps 全注入（api / dispatch / onEvent / sleep / persistence），单测用 fake api 驱动真分支。
 * - mode="real"：download_start → 等终态（log_line 冻结格式，见 CONTRACT-AUDIT）→
 *   verify_sha256 → exec_install → probe_run → TASK_DONE + 快照 → 下一任务 → ALL_DONE。
 * - mode="mock"：保持行为（阶段定时器 + 场景注入 net_dead/hash_mismatch）作为回落。
 * - 失败 → ERROR{code, messageKey, taskId}（E1 连败由 App 既有 effect 触发）；
 *   stop() 中止循环并取消在途下载（不派发 ERROR，由调用方决定）。
 */
import { isMock, type AiboxEvent, type DownloadTask, type ExecTask, type ProbeSpec } from "../net/client";
import type { Event, SessionSnapshot, Task } from "./types";

/* ── 错误码 → 人话 i18n key（ErrorOverlay t(messageKey)） ── */
const CODE_KEYS: Array<[prefix: string, key: string]> = [
  ["NET_ALL_CHANNELS_DEAD", "err.netDead"],
  ["NET_HASH_MISMATCH", "err.hashMismatch"],
  ["HASH_MISMATCH", "err.hashMismatch"],
  ["USER_CANCELLED", "err.cancelled"],
  ["SCAN_FAILED", "err.scanFailed"],
  ["REGISTRY_FETCH", "err.registry"],
  ["EXEC_", "err.execFailed"],
  ["PROBE_", "err.probeFailed"],
  ["DOWNLOAD_", "err.downloadFailed"],
  ["VERIFY_", "err.downloadFailed"],
  ["NET_", "err.downloadFailed"],
];

export function messageKeyForCode(code: string): string {
  for (const [prefix, key] of CODE_KEYS) {
    if (code === prefix || code.startsWith(prefix)) return key;
  }
  return "err.generic";
}

/**
 * 下载类错误（有「另一个源」可换 → 展示「换个下载源」按钮）：
 * `NET_*` / `DOWNLOAD_*` / 哈希不符。扫描、配置、执行类错误不适用。
 */
export function canSwitchSource(code: string): boolean {
  return (
    code.startsWith("NET_") ||
    code.startsWith("DOWNLOAD_") ||
    code.includes("HASH_MISMATCH")
  );
}

/** Rust commands.rs 下载终态 log_line（冻结格式；审计确认） */
const DL_OK = /^download ok:/;
const DL_FAILED = /^download failed \[([A-Z0-9_]+)\]/;
const DL_CANCELLED = /^download cancelled/;
const DL_PANIC = /^download panicked/;

export interface ExecuteApi {
  downloadStart(task: DownloadTask): Promise<string>;
  downloadCancel(jobId: string): Promise<void>;
  verifySha256(path: string, expected: string): Promise<boolean>;
  execInstall(task: ExecTask): Promise<{ code: number; out: string }>;
  probeRun(spec: ProbeSpec): Promise<{ version: string }>;
  /** 安装上报（best-effort：fire-and-forget，reject/同步抛都不影响编排；可选） */
  trackInstall?(appId: string): Promise<void>;
}

export interface ExecuteDeps {
  mode: "mock" | "real";
  api: ExecuteApi;
  dispatch: (e: Event) => void;
  onEvent: (h: (e: AiboxEvent) => void) => () => void;
  sleep: (ms: number) => Promise<void>;
  /** mock 场景开关（仅 mock 分支读取） */
  scenario: () => string;
  saveSnapshot: (snap: SessionSnapshot) => Promise<void>;
  now: () => number;
  /** 下载无任何事件超过该时长 → DOWNLOAD_STALLED（默认 3 分钟） */
  stallMs?: number;
  /** 「换个下载源」：消费一次性跳过通道列表（取后清空；缺省 = 无跳过） */
  takeSkipChannels?: () => string[];
}

export interface ExecuteContext {
  appId: string | null;
  /** 进入 EXECUTE 时的任务快照（done/skipped 跳过 = 断点续装语义） */
  tasks: Task[];
}

export interface ExecuteHandle {
  /** 中止循环 + 取消在途下载；不派发 ERROR（调用方负责语义） */
  stop(): void;
  promise: Promise<void>;
}

type RunResult = "done" | "stopped" | "error";

function downloadPath(destName: string): string {
  return `~/.aibox/cache/downloads/${destName}`;
}

/**
 * 等一次下载的终态（成功/失败/取消/停滞）。
 * 订阅先于 download_start，避免终态 log 先于订阅到达。
 */
function waitDownload(
  deps: ExecuteDeps,
  state: { stopped: boolean; lastEventAt: number; abortWait: (() => void) | null },
): Promise<{ ok: true } | { ok: false; code: string }> {
  return new Promise((resolve) => {
    let settled = false;
    const finish = (r: { ok: true } | { ok: false; code: string }) => {
      if (settled) return;
      settled = true;
      off();
      clearInterval(watchdog);
      if (state.abortWait === abort) state.abortWait = null;
      resolve(r);
    };
    const abort = () => finish({ ok: false, code: "USER_CANCELLED" });
    state.abortWait = abort;
    const off = deps.onEvent((e) => {
      if (state.stopped) return finish({ ok: false, code: "USER_CANCELLED" });
      if (e.event === "download_progress" || e.event === "log_line") {
        state.lastEventAt = deps.now();
      }
      if (e.event !== "log_line") return;
      const t = e.payload.text;
      if (DL_OK.test(t)) return finish({ ok: true });
      const m = t.match(DL_FAILED);
      if (m) return finish({ ok: false, code: m[1] });
      if (DL_PANIC.test(t)) return finish({ ok: false, code: "DOWNLOAD_PANIC" });
      if (DL_CANCELLED.test(t)) return finish({ ok: false, code: "USER_CANCELLED" });
    });
    const stallMs = deps.stallMs ?? 180_000;
    const watchdog = setInterval(() => {
      if (state.stopped) return finish({ ok: false, code: "USER_CANCELLED" });
      if (deps.now() - state.lastEventAt >= stallMs) {
        return finish({ ok: false, code: "DOWNLOAD_STALLED" });
      }
    }, Math.min(stallMs, 1000));
  });
}

export function startExecute(deps: ExecuteDeps, ctx: ExecuteContext): ExecuteHandle {
  const state = {
    stopped: false,
    jobId: null as string | null,
    lastEventAt: deps.now(),
    abortWait: null as (() => void) | null,
  };

  const stop = () => {
    state.stopped = true;
    // 打断在途下载等待（否则 promise 悬挂）
    state.abortWait?.();
    const j = state.jobId;
    state.jobId = null;
    if (j) void deps.api.downloadCancel(j).catch(() => undefined);
  };

  const fail = (code: string, taskId?: string): RunResult => {
    deps.dispatch({
      type: "ERROR",
      code,
      messageKey: messageKeyForCode(code),
      ...(taskId ? { taskId } : {}),
    });
    return "error";
  };

  const snapshot = async (doneIds: string[]) => {
    await deps
      .saveSnapshot({
        appId: ctx.appId ?? "",
        doneTaskIds: doneIds,
        savedAt: deps.now(),
      })
      .catch(() => undefined);
  };

  /* ── mock 回落：阶段定时器 + 场景注入（行为原样保留） ── */
  const runTaskMock = async (task: Task, doneIds: string[]): Promise<RunResult> => {
    const phases = ["download", "verify", "install", "probe"] as const;
    for (const phase of phases) {
      if (state.stopped) return "stopped";
      const sc = deps.scenario();
      if (sc === "net_dead" && phase === "download") {
        return fail("NET_ALL_CHANNELS_DEAD", task.id);
      }
      if (sc === "hash_mismatch" && phase === "verify") {
        return fail("HASH_MISMATCH", task.id);
      }
      deps.dispatch({ type: "TASK_PHASE", taskId: task.id, phase });
      await deps.sleep(180);
    }
    if (state.stopped) return "stopped";
    deps.dispatch({ type: "TASK_DONE", taskId: task.id });
    doneIds.push(task.id);
    await snapshot(doneIds);
    return "done";
  };

  /* ── 真分支：事件驱动四阶段 ── */
  const runTaskReal = async (task: Task, doneIds: string[]): Promise<RunResult> => {
    const spec = task.spec;
    if (!spec) return fail("TASK_NO_SPEC", task.id);

    // 1) download（「换个下载源」→ 一次性 skipChannels 重发）
    deps.dispatch({ type: "TASK_PHASE", taskId: task.id, phase: "download" });
    const skip = deps.takeSkipChannels?.() ?? [];
    const waiter = waitDownload(deps, state);
    try {
      state.jobId = await deps.api.downloadStart({
        ...spec.download,
        ...(skip.length > 0 ? { skipChannels: skip } : {}),
      });
    } catch (e) {
      return fail(errCode(e, "DOWNLOAD_START_FAILED"), task.id);
    }
    const outcome = await waiter;
    state.jobId = null;
    if (state.stopped) return "stopped";
    if (!outcome.ok) {
      if (outcome.code === "USER_CANCELLED") return "stopped";
      return fail(outcome.code, task.id);
    }

    // 2) verify（SHA256 零豁免，S1）
    if (state.stopped) return "stopped";
    deps.dispatch({ type: "TASK_PHASE", taskId: task.id, phase: "verify" });
    let verified: boolean;
    try {
      verified = await deps.api.verifySha256(
        downloadPath(spec.download.destName),
        spec.download.sha256,
      );
    } catch (e) {
      return fail(errCode(e, "VERIFY_FAILED"), task.id);
    }
    if (!verified) return fail("HASH_MISMATCH", task.id);

    // 3) install（静默、零提权；exit≠0 → EXEC_FAILED）
    if (state.stopped) return "stopped";
    deps.dispatch({ type: "TASK_PHASE", taskId: task.id, phase: "install" });
    let exit: { code: number; out: string };
    try {
      exit = await deps.api.execInstall(spec.exec);
    } catch (e) {
      return fail(errCode(e, "EXEC_SPAWN_FAILED"), task.id);
    }
    if (exit.code !== 0) return fail(`EXEC_EXIT_${exit.code}`, task.id);

    // 4) probe（拿到真实版本号才算装上；无 probe 定义则通过）
    if (state.stopped) return "stopped";
    deps.dispatch({ type: "TASK_PHASE", taskId: task.id, phase: "probe" });
    if (spec.probe) {
      try {
        await deps.api.probeRun(spec.probe);
      } catch (e) {
        return fail(errCode(e, "PROBE_MISMATCH"), task.id);
      }
    }

    if (state.stopped) return "stopped";
    deps.dispatch({ type: "TASK_DONE", taskId: task.id });
    doneIds.push(task.id);
    await snapshot(doneIds);
    return "done";
  };

  const promise = (async (): Promise<void> => {
    const doneIds = ctx.tasks
      .filter((t) => t.status === "done" || t.status === "skipped")
      .map((t) => t.id);
    // 安装上报：任务队列真正启动（首个待执行任务）时点一次；
    // 同轮幂等（tracked 门控），失败静默绝不阻塞主流程。
    let tracked = false;
    for (const task of ctx.tasks) {
      if (state.stopped) return;
      if (task.status === "done" || task.status === "skipped") continue;
      if (!tracked && ctx.appId) {
        tracked = true;
        try {
          void deps.api.trackInstall?.(ctx.appId)?.catch(() => undefined);
        } catch {
          /* 同步抛也吞（best-effort） */
        }
      }
      const result =
        deps.mode === "mock" ? await runTaskMock(task, doneIds) : await runTaskReal(task, doneIds);
      if (result !== "done") return;
    }
    if (state.stopped) return;
    deps.dispatch({ type: "ALL_DONE" });
  })();

  return { stop, promise };
}

/** Tauri invoke reject / Error → 机器可读 code */
function errCode(e: unknown, fallback: string): string {
  if (e && typeof e === "object" && "code" in e && typeof (e as { code: unknown }).code === "string") {
    return (e as { code: string }).code;
  }
  return fallback;
}

/** 便捷构造（App 用真实 api 接线） */
export function realDeps(
  api: ExecuteApi,
  base: Omit<ExecuteDeps, "mode" | "api">,
): ExecuteDeps {
  return { mode: isMock() ? "mock" : "real", api, ...base };
}
