/**
 * Selector：屏幕映射、进度、导航六态。
 */
import { FLOW, TASK_PHASES, type MachineState, type ScanReport, type ScanRow, type Status, type Task } from "./types";

/** 屏幕映射：S1=IDLE … S7=DONE，ERROR=覆盖层 */
export type ScreenId =
  | "S1Idle"
  | "S2Scan"
  | "S3Plan"
  | "S4Execute"
  | "S5Config"
  | "S6Verify"
  | "S7Done";

export function screenFor(status: Status): ScreenId {
  switch (status) {
    case "IDLE":
      return "S1Idle";
    case "SCAN":
      return "S2Scan";
    case "PLAN":
    case "CONFIRM":
      return "S3Plan";
    case "EXECUTE":
      return "S4Execute";
    case "CONFIG":
      return "S5Config";
    case "VERIFY":
      return "S6Verify";
    case "DONE":
      return "S7Done";
    case "ERROR":
      // ERROR 为覆盖层，底层屏幕保持出错前一屏
      return "S4Execute";
  }
}

/** 左侧 64px 导航的六态圆点（IDLE→VERIFY，DONE 归入 VERIFY 之后的完成点） */
export const NAV_STEPS: readonly Status[] = [
  "IDLE",
  "SCAN",
  "PLAN",
  "EXECUTE",
  "CONFIG",
  "VERIFY",
] as const;

/** 当前导航高亮 index（ERROR 时高亮出错前的态；DONE 高亮最后一格） */
export function navIndex(status: Status, errorFrom?: Status): number {
  const effective: Status =
    status === "ERROR" ? (errorFrom ?? "EXECUTE") : status;
  if (effective === "DONE" || effective === "CONFIRM") {
    // CONFIRM 属 PLAN 步；DONE 属 VERIFY 之后的完成态
    return effective === "DONE" ? NAV_STEPS.length - 1 : NAV_STEPS.indexOf("PLAN");
  }
  const i = NAV_STEPS.indexOf(effective);
  return i < 0 ? 0 : i;
}

export function currentTask(s: MachineState): Task | null {
  if (s.status !== "EXECUTE") return null;
  return s.tasks[s.currentTaskIndex] ?? null;
}

export function completedTasks(s: MachineState): Task[] {
  return s.tasks.filter((t) => t.status === "done" || t.status === "skipped");
}

/** EXECUTE 总进度（按阶段粒度：每任务 4 阶段） */
export function progress(s: MachineState): { done: number; total: number; pct: number } {
  const total = s.tasks.length * TASK_PHASES.length;
  if (total === 0) return { done: 0, total: 0, pct: 0 };
  let done = 0;
  for (const t of s.tasks) {
    if (t.status === "done" || t.status === "skipped") {
      done += TASK_PHASES.length;
    } else if (t.status === "running") {
      done += TASK_PHASES.indexOf(t.phase);
    }
  }
  return { done, total, pct: Math.round((done / total) * 100) };
}

/** ERROR 覆盖层的三按钮可用性（重试/跳过/导出诊断） */
export function errorActions(s: MachineState): {
  retry: boolean;
  skip: boolean;
  exportDiag: boolean;
} {
  if (s.status !== "ERROR" || !s.error) {
    return { retry: false, skip: false, exportDiag: false };
  }
  const from = s.error.from;
  return {
    retry: true,
    // SKIP 仅在 EXECUTE 有可跳任务时可用
    skip: from === "EXECUTE" && s.currentTaskIndex < s.tasks.length,
    exportDiag: true,
  };
}

/** 是否处于主流程（导航可点）—— ERROR 时底层仍算流程态 */
export function inFlow(status: Status): boolean {
  return FLOW.includes(status) || status === "ERROR";
}

/** 断点续装：是否有可恢复的未完成任务边界 */
export function hasResumableSession(s: MachineState): boolean {
  return s.resume.available && s.resume.prompt && s.resume.snapshot !== null;
}

/**
 * ScanReport → S2 左列行（契约核对：checks 为真源，顶层字段派生 value 列）。
 * hint 仅对存在 `scan.hint.<id>` 语言包的项给出（磁盘/节点/端口/WSL 四项有解释文案）。
 */
const HINT_IDS = new Set(["disk", "node", "port", "wsl"]);

export function scanRows(report: ScanReport | null): ScanRow[] {
  if (!report) return [];
  const v = (id: string): string => {
    switch (id) {
      case "os":
        return `${report.os.platform} ${report.os.version}`;
      case "arch":
        return report.os.arch;
      case "disk":
        return `${report.diskFreeGb} GB`;
      case "mem":
        return `${report.memTotalGb} GB`;
      case "node":
        return report.node.version ?? "—";
      case "git":
        return report.git.version ?? "—";
      case "wsl":
        return report.wsl.installed ? (report.wsl.distros?.join(", ") || "√") : "—";
      case "port":
        return report.port18789.occupied
          ? report.port18789.process || "127.0.0.1:18789"
          : "free";
      case "installed":
        return report.installedApps.length > 0
          ? report.installedApps.map((a) => a.version ?? a.id).join(", ")
          : "—";
      default:
        return "";
    }
  };
  return report.checks.map((c) => ({
    id: c.id,
    labelKey: c.human,
    level: c.level,
    ok: c.level === "ok",
    value: v(c.id),
    ...(HINT_IDS.has(c.id) ? { hintKey: `scan.hint.${c.id}` } : {}),
    // node 不支持的版本：Rust 注入 note（i18n key），作为附加解释
    ...(c.id === "node" && report.node.note ? { noteKey: report.node.note } : {}),
  }));
}

/** S1 侧栏前 5 行（与 S2 同源） */
export function sidebarScanRows(report: ScanReport | null): ScanRow[] {
  return scanRows(report).slice(0, 5);
}
