/**
 * 安装状态机类型。
 * 本文件与 reducer/selectors 均为纯函数层，不依赖 React / Tauri。
 * 扫描报告 = wire 类型（契约核对统一）。
 */
import type { DownloadTask, ExecTask, ProbeSpec, ScanReport } from "../net/client";

/** 单任务四阶段执行载荷（PLAN 构建时由 registry packages 展开） */
export interface TaskSpec {
  download: DownloadTask;
  exec: ExecTask;
  probe?: ProbeSpec;
}

export type { ScanReport };

/** 八态 + ERROR（ERROR 为覆盖层态，不在 FLOW 顺序里） */
export type Status =
  | "IDLE"
  | "SCAN"
  | "PLAN"
  | "CONFIRM"
  | "EXECUTE"
  | "CONFIG"
  | "VERIFY"
  | "DONE"
  | "ERROR";

/** 主流程顺序（左侧 6 步导航用 IDLE→VERIFY，DONE 为终态收尾） */
export const FLOW: readonly Status[] = [
  "IDLE",
  "SCAN",
  "PLAN",
  "CONFIRM",
  "EXECUTE",
  "CONFIG",
  "VERIFY",
  "DONE",
] as const;

/** EXECUTE 单任务四阶段 */
export type TaskPhase = "download" | "verify" | "install" | "probe";
export const TASK_PHASES: readonly TaskPhase[] = [
  "download",
  "verify",
  "install",
  "probe",
] as const;

export type TaskStatus = "pending" | "running" | "done" | "failed" | "skipped";

/** 安装计划中的一个任务（= 一个组件的 下载→校验→安装→探测） */
export interface Task {
  id: string;
  name: string;
  version: string;
  phase: TaskPhase;
  status: TaskStatus;
  /** 四阶段执行载荷（PLAN 构建时注入；纯 reducer 测试可缺省） */
  spec?: TaskSpec;
}

/** 体检报告左列行（由 ScanReport.checks + 顶层字段派生，S2/S1 侧栏共用） */
export interface ScanRow {
  id: string;
  labelKey: string;
  ok: boolean;
  level: "ok" | "warn" | "fail";
  /** 人话解释（红灯项点击展开，S2 规格） */
  hintKey?: string;
  value?: string;
  /** 附加说明 i18n key（如 node 版本不受支持：scan.node.unsupported） */
  noteKey?: string;
}

/** DAG 节点三色：reuse=已满足(--ok) / install=将安装(--info) / skip=无需(灰) */
export type PlanNodeKind = "target" | "install" | "reuse" | "skip";

export interface PlanNode {
  id: string;
  name: string;
  versionRange: string;
  kind: PlanNodeKind;
  children: string[];
}

export interface Plan {
  appId: string;
  nodes: PlanNode[];
  tasks: Task[];
  /** 路线（CONFIRM 页明示路线与通道） */
  route: string;
  channel: string;
  diskMb: number;
  estMinutes: number;
  /** 区域判定来源（诊断：last-plan.json 里一眼可见当时生效判定） */
  netRegion?: { source: string; effective: "cn" | "global" };
}

/** 任意态 → ERROR 携带的人话错误码 */
export interface AppError {
  code: string;
  messageKey: string;
  /** 出错时所在主流程态（RETRY/SKIP/EXPORT_DIAG 恢复用） */
  from: Status;
  taskId?: string;
}

/** 断点续装快照（EXECUTE 每任务完成持久化） */
export interface SessionSnapshot {
  appId: string;
  doneTaskIds: string[];
  savedAt: number;
}

export interface ResumeState {
  /** 是否检测到未完成会话 */
  available: boolean;
  snapshot: SessionSnapshot | null;
  /** 「继续上次安装？」弹层是否展示 */
  prompt: boolean;
}

export interface MachineState {
  status: Status;
  appId: string | null;
  scan: ScanReport | null;
  plan: Plan | null;
  tasks: Task[];
  currentTaskIndex: number;
  error: AppError | null;
  resume: ResumeState;
  /** CONFIG 已选 provider 路径 id（S5 C1） */
  configPathId: string | null;
  verifyOk: boolean;
}

/** 事件契约（签名即文档） */
export type Event =
  | { type: "START_INSTALL" } // IDLE → SCAN
  | { type: "SCAN_DONE"; report: ScanReport } // SCAN → PLAN
  | { type: "PLAN_READY"; plan: Plan } // PLAN 自持（数据注入）
  | { type: "CONFIRM" } // PLAN → CONFIRM
  | { type: "BEGIN" } // CONFIRM → EXECUTE
  | { type: "TASK_PHASE"; taskId: string; phase: TaskPhase } // EXECUTE 内推进
  | { type: "TASK_DONE"; taskId: string } // EXECUTE 内任务完成（→持久化快照）
  | { type: "ALL_DONE" } // EXECUTE → CONFIG
  | { type: "CONFIG_WRITTEN"; pathId: string } // CONFIG → VERIFY
  | { type: "VERIFY_OK" } // VERIFY → DONE
  | { type: "ERROR"; code: string; messageKey: string; taskId?: string } // 任意态 → ERROR
  | { type: "RETRY" } // ERROR → 出错原态
  | { type: "SKIP" } // ERROR → 出错原态（跳过失败任务）
  | { type: "EXPORT_DIAG" } // ERROR → 出错原态（导出为副作用）
  | { type: "CONFIG_SELECT"; pathId: string } // CONFIG 内选路径（不转移状态）
  | { type: "BACK" } // 主流程回退一步（仅 IDLE/SCAN/PLAN 可回）
  | { type: "DETECT_SESSION"; snapshot: SessionSnapshot } // IDLE 检测未完成会话
  | { type: "RESUME_SESSION" } // 恢复到 EXECUTE 该任务边界
  | { type: "DISMISS_RESUME" } // 放弃继续
  | { type: "RESTART" }; // DONE → IDLE（再装一个）

export function createInitialState(): MachineState {
  return {
    status: "IDLE",
    appId: null,
    scan: null,
    plan: null,
    tasks: [],
    currentTaskIndex: 0,
    error: null,
    resume: { available: false, snapshot: null, prompt: false },
    configPathId: null,
    verifyOk: false,
  };
}
