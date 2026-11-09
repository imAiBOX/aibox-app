/**
 * 状态机 reducer：纯函数，全分支可测。
 *
 * 转移表：
 *   IDLE →(START_INSTALL)→ SCAN →(SCAN_DONE)→ PLAN →(CONFIRM)→ CONFIRM
 *        →(BEGIN)→ EXECUTE →(ALL_DONE)→ CONFIG →(CONFIG_WRITTEN)→ VERIFY
 *        →(VERIFY_OK)→ DONE
 *   任意态 →(ERROR{human_error_code})→ ERROR；ERROR →(RETRY|SKIP|EXPORT_DIAG)→ 出错原态
 *   IDLE 检测未完成会话(DETECT_SESSION) → 弹「继续上次安装？」→ RESUME_SESSION → EXECUTE 边界
 *
 * 不合法事件（当前态不接受）→ 返回原状态（幂等，不抛错）。
 */
import {
  createInitialState,
  type Event,
  type MachineState,
  type Status,
  type Task,
} from "./types";

export { createInitialState };

function setError(
  s: MachineState,
  code: string,
  messageKey: string,
  taskId?: string,
): MachineState {
  return {
    ...s,
    status: "ERROR",
    error: { code, messageKey, from: s.status, taskId },
  };
}

/** 从 ERROR 恢复到出错原态（RETRY/SKIP/EXPORT_DIAG 共用） */
function recover(s: MachineState, opts: { skipFailed?: boolean } = {}): MachineState {
  if (s.status !== "ERROR" || !s.error) return s;
  const from: Status = s.error.from;
  let tasks = s.tasks;
  let idx = s.currentTaskIndex;
  if (opts.skipFailed && from === "EXECUTE") {
    tasks = s.tasks.map((t, i) =>
      i === s.currentTaskIndex && t.status === "failed"
        ? { ...t, status: "skipped" as const }
        : t,
    );
    idx = Math.min(s.currentTaskIndex + 1, s.tasks.length);
  }
  return {
    ...s,
    status: from,
    error: null,
    tasks,
    currentTaskIndex: idx,
  };
}

/** 恢复会话：定位到第一个未完成任务的边界（断点续装语义） */
function resumeToExec(
  s: MachineState,
  appId: string,
  doneTaskIds: string[],
  plan: import("./types").Plan | null,
): MachineState {
  const tasks: Task[] =
    plan && plan.appId === appId
      ? plan.tasks.map((t) =>
          doneTaskIds.includes(t.id)
            ? { ...t, status: "done" as const }
            : t,
        )
      : s.tasks.map((t) =>
          doneTaskIds.includes(t.id) ? { ...t, status: "done" as const } : t,
        );
  const firstOpen = tasks.findIndex((t) => t.status !== "done");
  return {
    ...s,
    status: "EXECUTE",
    appId,
    tasks,
    currentTaskIndex: firstOpen < 0 ? tasks.length : firstOpen,
    error: null,
    resume: { available: false, snapshot: null, prompt: false },
  };
}

export function reduce(state: MachineState, event: Event): MachineState {
  switch (event.type) {
    case "START_INSTALL":
      return state.status === "IDLE"
        ? { ...state, status: "SCAN", error: null }
        : state;

    case "SCAN_DONE":
      return state.status === "SCAN"
        ? { ...state, status: "PLAN", scan: event.report, error: null }
        : state;

    case "PLAN_READY":
      // PLAN 态内数据注入（自转移）；任务队列以 plan.tasks 为准
      return state.status === "PLAN"
        ? {
            ...state,
            plan: event.plan,
            appId: event.plan.appId,
            tasks: event.plan.tasks,
            currentTaskIndex: 0,
          }
        : state;

    case "CONFIRM":
      return state.status === "PLAN"
        ? { ...state, status: "CONFIRM", error: null }
        : state;

    case "BEGIN":
      return state.status === "CONFIRM"
        ? { ...state, status: "EXECUTE", currentTaskIndex: 0, error: null }
        : state;

    case "TASK_PHASE": {
      if (state.status !== "EXECUTE") return state;
      const tasks = state.tasks.map((t) =>
        t.id === event.taskId ? { ...t, phase: event.phase, status: "running" as const } : t,
      );
      const idx = tasks.findIndex((t) => t.id === event.taskId);
      return { ...state, tasks, currentTaskIndex: idx < 0 ? state.currentTaskIndex : idx };
    }

    case "TASK_DONE": {
      if (state.status !== "EXECUTE") return state;
      const tasks = state.tasks.map((t) =>
        t.id === event.taskId ? { ...t, status: "done" as const, phase: "probe" as const } : t,
      );
      const idx = tasks.findIndex((t) => t.id === event.taskId);
      const allDone = tasks.every((t) => t.status === "done");
      return {
        ...state,
        tasks,
        currentTaskIndex: idx < 0 ? state.currentTaskIndex : idx,
        // 全部完成时保持 EXECUTE，等待 ALL_DONE（由 UI/编排层发）
        ...(allDone ? {} : {}),
      };
    }

    case "ALL_DONE":
      return state.status === "EXECUTE" &&
        state.tasks.length > 0 &&
        state.tasks.every((t) => t.status === "done")
        ? { ...state, status: "CONFIG", error: null }
        : state;

    case "CONFIG_WRITTEN":
      return state.status === "CONFIG"
        ? { ...state, status: "VERIFY", configPathId: event.pathId, error: null }
        : state;

    case "VERIFY_OK":
      return state.status === "VERIFY"
        ? { ...state, status: "DONE", verifyOk: true, error: null }
        : state;

    case "ERROR":
      return setError(state, event.code, event.messageKey, event.taskId);

    case "RETRY":
      return recover(state);

    case "SKIP":
      return recover(state, { skipFailed: true });

    case "EXPORT_DIAG":
      // 导出是副作用（调用方执行），状态机只负责恢复出错原态
      return recover(state);

    case "CONFIG_SELECT":
      // CONFIG 态内选 provider 路径（自转移，不写入）
      return state.status === "CONFIG"
        ? { ...state, configPathId: event.pathId }
        : state;

    case "BACK": {
      // 仅早期态可回退（SCAN→IDLE、PLAN→SCAN、CONFIRM→PLAN）
      const back: Partial<Record<Status, Status>> = {
        SCAN: "IDLE",
        PLAN: "SCAN",
        CONFIRM: "PLAN",
      };
      const prev = back[state.status];
      return prev ? { ...state, status: prev, error: null } : state;
    }

    case "DETECT_SESSION":
      // 仅 IDLE 态响应；弹「继续上次安装？」
      return state.status === "IDLE"
        ? {
            ...state,
            resume: { available: true, snapshot: event.snapshot, prompt: true },
          }
        : state;

    case "RESUME_SESSION": {
      if (state.status !== "IDLE" || !state.resume.snapshot) return state;
      const snap = state.resume.snapshot;
      return resumeToExec(state, snap.appId, snap.doneTaskIds, state.plan);
    }

    case "DISMISS_RESUME":
      return state.resume.prompt
        ? { ...state, resume: { ...state.resume, prompt: false } }
        : state;

    case "RESTART":
      return state.status === "DONE"
        ? {
            ...createInitialState(),
            // 保留体检结果与已装列表到下次 S1 状态徽章展示
            scan: state.scan,
          }
        : state;

    default: {
      // 穷尽性检查：event 类型必须全部处理
      const _never: never = event;
      void _never;
      return state;
    }
  }
}
