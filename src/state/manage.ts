/**
 * MANAGE 维护模式状态机（纯 reducer；与安装八态互不干扰——独立状态组）。
 *
 * 转移表（产品规格 §5）：
 *   MANAGE →(UPDATE)→ UPDATE →(M_ALL_DONE|M_ERROR)→ DONE / ERROR
 *   MANAGE →(风险卡确认)→ REMOVE | REINSTALL →(完成|M_ERROR)→ DONE / ERROR
 *   ERROR →(M_RETRY)→ 出错原态；DONE/ERROR →(M_CLOSE)→ MANAGE
 *
 * 硬规则（风险卡守卫，reducer 层强制）：
 * ① 备份默认勾选（registry risk.backupFirst，缺省 true）；
 * ② 不勾备份 → 必须二次确认（noBackupAck）才能执行卸载/重装；
 * ③ 取消 = 不触发（status 留在 MANAGE、risk 清空）；
 * ④ 路径清单只来自 registry maintenance（dataPaths/residualPaths/steps），组装见 buildRemovePaths。
 *
 * 不合法事件（当前态不接受）→ 返回原状态（幂等，不抛错）。
 */
import type { Task, TaskPhase } from "./types";

/** 维护操作（与安装八态正交） */
export type ManageOp = "UPDATE" | "REMOVE" | "REINSTALL";

/** MANAGE 状态组：MANAGE(列表/待机) → 操作态 → DONE / ERROR */
export type ManageStatus = "MANAGE" | ManageOp | "DONE" | "ERROR";

/** 风险卡状态（破坏性操作确认） */
export interface RiskState {
  op: Exclude<ManageOp, "UPDATE">;
  appId: string;
  /** 备份勾选（默认 = registry risk.backupFirst，缺省 true） */
  backup: boolean;
  /** 不勾备份时的二次确认（「我明白不备份的风险」） */
  noBackupAck: boolean;
  /** registry risk.onUninstall 人话原文（展示用，zh/en 同串由 registry 提供） */
  riskText?: string;
  /** 会被删除的用户数据路径（人话展示；只来自 registry dataPaths） */
  dataPaths: string[];
  /** 会被删除的应用残留路径（registry residualPaths）——同样有破坏性，必须展示 */
  residualPaths?: string[];
  /** registry 声明的删除步骤（steps）——同样属于破坏性清单 */
  steps?: string[];
}

export interface ManageError {
  code: string;
  messageKey: string;
  /** 出错时所在操作态（M_RETRY 恢复用） */
  from: ManageOp;
}

export interface ManageState {
  status: ManageStatus;
  appId: string | null;
  op: ManageOp | null;
  /** 风险卡（null = 未打开） */
  risk: RiskState | null;
  /** 本次破坏性操作是否备份（风险卡确认时定格；false = 用户二次确认不备份） */
  backup: boolean;
  /** 卸载/备份结果（DONE 页展示备份位置） */
  backupDir: string | null;
  removed: string[];
  skipped: { path: string; reason: string }[];
  /** UPDATE/REINSTALL 安装链任务进度（复用 EXECUTE 视觉） */
  tasks: Task[];
  currentTaskIndex: number;
  error: ManageError | null;
}

/** registry maintenance 子集（只读需要的字段；形状与 registry app.json 一致） */
export interface MaintenanceInfo {
  upgrade?: { type?: string; probe?: unknown; notes?: string };
  uninstall?: { steps?: string[]; residualPaths?: string[]; notes?: string };
  dataPaths?: string[];
  risk?: { onUninstall?: string; backupFirst?: boolean };
}

/** 路径清单组装（硬规则④）：只来自 registry maintenance，绝不掺前端自造路径。 */
export function buildRemovePaths(m: MaintenanceInfo | null | undefined): {
  dataPaths: string[];
  residualPaths: string[];
  steps: string[];
  /** 备份默认开（risk.backupFirst 缺省 true） */
  backupFirst: boolean;
  riskText?: string;
} {
  const dataPaths = Array.isArray(m?.dataPaths) ? [...m.dataPaths] : [];
  const residualPaths = Array.isArray(m?.uninstall?.residualPaths)
    ? [...m.uninstall.residualPaths]
    : [];
  const steps = Array.isArray(m?.uninstall?.steps) ? [...m.uninstall.steps] : [];
  const backupFirst = m?.risk?.backupFirst ?? true; // 缺省 true = 硬规则①
  const riskText = m?.risk?.onUninstall;
  return { dataPaths, residualPaths, steps, backupFirst, ...(riskText ? { riskText } : {}) };
}

/**
 * 两份破坏性清单是否相同（纯函数，单测）。
 *
 * **三项都要比**（数据路径 / 应用残留路径 / 人工步骤）：
 * 只比 dataPaths 会让「只有残留路径变了」被当成没变 → 用户看到的清单与实际执行的不一致。
 */
export function sameManifest(
  a: { dataPaths: string[]; residualPaths: string[]; steps: string[] },
  b: { dataPaths: string[]; residualPaths: string[]; steps: string[] },
): boolean {
  const norm = (xs: string[]) => JSON.stringify([...xs].sort());
  return (
    norm(a.dataPaths) === norm(b.dataPaths) &&
    norm(a.residualPaths) === norm(b.residualPaths) &&
    norm(a.steps) === norm(b.steps)
  );
}

/** 风险卡能否执行（纯守卫，单测）：不勾备份 → 必须二次确认。 */
export function canConfirmRisk(risk: RiskState): boolean {
  return risk.backup || risk.noBackupAck;
}

export type ManageEvent =
  | { type: "M_OPEN" } // 关闭/复位 → MANAGE
  | { type: "M_RISK_OPEN"; op: Exclude<ManageOp, "UPDATE">; appId: string; backupFirst: boolean; riskText?: string; dataPaths?: string[]; residualPaths?: string[]; steps?: string[] }
  | { type: "M_RISK_BACKUP"; checked: boolean } // 备份勾选（默认勾）
  | { type: "M_RISK_NO_BACKUP_ACK"; acked: boolean } // 不勾时二次确认
  | { type: "M_RISK_CANCEL" } // 取消 = 不触发
  | { type: "M_RISK_CONFIRM" } // 确认 → 进入 REMOVE/REINSTALL（守卫不过 → 原态）
  | { type: "M_BEGIN_UPDATE"; appId: string } // 升级（非破坏性，免风险卡）
  | { type: "M_TASKS_READY"; tasks: Task[] } // 升级/重装安装链注入
  | { type: "M_TASK_PHASE"; taskId: string; phase: TaskPhase }
  | { type: "M_TASK_DONE"; taskId: string }
  | { type: "M_ALL_DONE" } // 安装链跑完 → DONE
  | { type: "M_REMOVE_DONE"; backupDir?: string; removed?: string[]; skipped?: { path: string; reason: string }[] }
  | { type: "M_ERROR"; code: string; messageKey: string }
  | { type: "M_RETRY" } // ERROR → 出错原态
  | { type: "M_CLOSE" }; // DONE/ERROR → MANAGE

export function createInitialManageState(): ManageState {
  return {
    status: "MANAGE",
    appId: null,
    op: null,
    risk: null,
    backup: true,
    backupDir: null,
    removed: [],
    skipped: [],
    tasks: [],
    currentTaskIndex: 0,
    error: null,
  };
}

/** 操作态（可执行安装链/卸载的状态集合） */
const OP_STATUSES: readonly ManageOp[] = ["UPDATE", "REMOVE", "REINSTALL"];

export function manageReduce(state: ManageState, event: ManageEvent): ManageState {
  switch (event.type) {
    case "M_OPEN":
      return createInitialManageState();

    case "M_RISK_OPEN":
      // MANAGE 态可开风险卡；**ERROR 态也允许**（重试时发现 registry 清单变了
      // → 要按新清单重新确认）。
      // **必须同时离开 ERROR 态**：只写 risk 不改 status 的话，
      // 界面在 ERROR 分支就提前 return（风险卡看不见），
      // 且 M_RISK_CONFIRM 只接受 MANAGE → 确认事件也被忽略 → 用户彻底卡死
      // （独立审计 2026-11-20 第三轮 REJECT）。
      // 注意：这里**会清掉当前错误**（error → null）。错误上下文不保留，
      // 组件侧只保留操作参数（attemptRef）；用户是主动点「重试」才走到这里的，
      // 语义上等于「重新走一遍确认流程」。
      // backupFirst 缺省 → 默认勾选备份（硬规则①）
      if (state.status !== "MANAGE" && state.status !== "ERROR") return state;
      return {
        ...state,
        status: "MANAGE",
        error: null,
        risk: {
          op: event.op,
          appId: event.appId,
          backup: event.backupFirst,
          noBackupAck: false,
          ...(event.riskText ? { riskText: event.riskText } : {}),
          dataPaths: event.dataPaths ?? [],
          residualPaths: event.residualPaths ?? [],
          steps: event.steps ?? [],
        },
      };

    case "M_RISK_BACKUP":
      if (!state.risk) return state;
      // 取消勾选 → 清掉旧的二次确认（必须重新确认）
      return {
        ...state,
        risk: {
          ...state.risk,
          backup: event.checked,
          ...(event.checked ? { noBackupAck: false } : {}),
        },
      };

    case "M_RISK_NO_BACKUP_ACK":
      if (!state.risk) return state;
      return { ...state, risk: { ...state.risk, noBackupAck: event.acked } };

    case "M_RISK_CANCEL":
      // 取消不触发：回 MANAGE、清风险卡、不动任何执行态
      if (state.status !== "MANAGE") return state;
      return { ...state, risk: null };

    case "M_RISK_CONFIRM": {
      if (state.status !== "MANAGE" || !state.risk) return state;
      // 守卫：不勾备份且未二次确认 → 拒绝执行（原态）
      if (!canConfirmRisk(state.risk)) return state;
      const { op, appId, backup } = state.risk;
      return {
        ...state,
        status: op,
        op,
        appId,
        risk: null,
        backup,
        error: null,
      };
    }

    case "M_BEGIN_UPDATE":
      // 升级 = 重跑安装链（非破坏性，无需风险卡）
      if (state.status !== "MANAGE") return state;
      return {
        ...state,
        status: "UPDATE",
        op: "UPDATE",
        appId: event.appId,
        error: null,
        tasks: [],
        currentTaskIndex: 0,
      };

    case "M_TASKS_READY":
      if (!OP_STATUSES.includes(state.status as ManageOp)) return state;
      return { ...state, tasks: event.tasks, currentTaskIndex: 0 };

    case "M_TASK_PHASE": {
      if (state.status !== "UPDATE" && state.status !== "REINSTALL") return state;
      const tasks = state.tasks.map((t) =>
        t.id === event.taskId ? { ...t, phase: event.phase, status: "running" as const } : t,
      );
      const idx = tasks.findIndex((t) => t.id === event.taskId);
      return { ...state, tasks, currentTaskIndex: idx < 0 ? state.currentTaskIndex : idx };
    }

    case "M_TASK_DONE": {
      if (state.status !== "UPDATE" && state.status !== "REINSTALL") return state;
      const tasks = state.tasks.map((t) =>
        t.id === event.taskId
          ? { ...t, status: "done" as const, phase: "probe" as const }
          : t,
      );
      const idx = tasks.findIndex((t) => t.id === event.taskId);
      return { ...state, tasks, currentTaskIndex: idx < 0 ? state.currentTaskIndex : idx };
    }

    case "M_ALL_DONE":
      // 安装链全部完成（含 skipped 视为已处置）→ DONE
      if (state.status !== "UPDATE" && state.status !== "REINSTALL") return state;
      if (state.tasks.length === 0) return state;
      if (!state.tasks.every((t) => t.status === "done" || t.status === "skipped")) {
        return state;
      }
      return { ...state, status: "DONE", error: null };

    case "M_REMOVE_DONE":
      // 卸载（REMOVE / REINSTALL 的卸载阶段）完成 → DONE
      if (state.status !== "REMOVE" && state.status !== "REINSTALL") return state;
      // REINSTALL：卸载完成不进 DONE，等安装链（由编排层注入 M_TASKS_READY）
      if (state.status === "REINSTALL") {
        return {
          ...state,
          backupDir: event.backupDir ?? state.backupDir,
          removed: event.removed ?? state.removed,
          skipped: event.skipped ?? state.skipped,
        };
      }
      return {
        ...state,
        status: "DONE",
        backupDir: event.backupDir ?? null,
        removed: event.removed ?? [],
        skipped: event.skipped ?? [],
        error: null,
      };

    case "M_ERROR": {
      if (!OP_STATUSES.includes(state.status as ManageOp)) return state;
      return {
        ...state,
        status: "ERROR",
        error: { code: event.code, messageKey: event.messageKey, from: state.status as ManageOp },
      };
    }

    case "M_RETRY": {
      if (state.status !== "ERROR" || !state.error) return state;
      return { ...state, status: state.error.from, error: null };
    }

    case "M_CLOSE":
      if (state.status !== "DONE" && state.status !== "ERROR") return state;
      return createInitialManageState();

    default: {
      // 穷尽性检查：事件类型必须全部处理
      const _never: never = event;
      void _never;
      return state;
    }
  }
}

/**
 * 错误码 → 人话 i18n key（**不把 Rust 英文原文当说明给用户**）。
 *
 * 旧版把异常 message（英文 "failed to remove file …: permission denied (os error 5)"）
 * 直接截 64 字符当「说明」显示，却又在文案里写「看下面的说明」——小白完全看不懂。
 * 这里按机器码分流；实在认不出的再按关键字嗅（权限 / 文件被占用）。
 *
 * **还原类错误码（`RESTORE_*`）各自独立**（P2-7）：它们全是「**整体拒绝**」——
 * 什么都没改动、只能人工取回；统一落 `s8.errRemove`（「可能部分改动」）是**误导**。
 */
/**
 * 错误码 → 人话 key 的**单一真源**（导出供还原流程复用，避免两处各写一份而漂移）。
 */
export const MANAGE_ERROR_KEYS: Record<string, string> = {
  MANAGE_NO_MANIFEST: "s8.errNoManifest",
  MANAGE_REMOVE_PAUSED: "s8.errRemovePaused",
  MANAGE_POLICY_CHANGED: "s8.errMaintenanceChanged",
  MANAGE_PATH_CHANGED: "s8.errMaintenanceChanged",
  MANAGE_REGISTRY_UNVERIFIED: "s8.errMaintenanceUnverified",
  MANAGE_POLICY_INVALID: "s8.errMaintenanceInvalid",
  REGISTRY_MISMATCH: "s8.errMaintenanceInvalid",
  REGISTRY_INTEGRITY: "s8.errMaintenanceInvalid",
  REGISTRY_BAD_JSON: "s8.errMaintenanceInvalid",
  REGISTRY_BAD_ID: "s8.errMaintenanceInvalid",
  MANAGE_UNSAFE_PATH: "s8.errOutOfScope",
  MANAGE_PROTECTED: "s8.errProtected",
  MANAGE_OUT_OF_SCOPE: "s8.errOutOfScope",
  MANAGE_SYMLINK_ESCAPE: "s8.errSymlinkEscape",
  MANAGE_BACKUP_FAILED: "s8.errBackupFailed",
  MANAGE_BACKUP_INCOMPLETE: "s8.errBackupFailed",
  MANAGE_DELETE_FAILED: "s8.errDeleteFailed",
  IO_ERROR: "s8.errIo",
  // ── 安全还原（`manage_restore`）：每码一句人话，都说清「什么都没改动」──
  RESTORE_NO_MANIFEST: "s8.errRestoreNoManifest",
  RESTORE_HOME_MISMATCH: "s8.errRestoreHomeMismatch",
  RESTORE_TOO_LARGE: "s8.errRestoreTooLarge",
  RESTORE_MANIFEST_TOO_LARGE: "s8.errRestoreManifestTooLarge",
  RESTORE_APP_MISMATCH: "s8.errRestoreAppMismatch",
  RESTORE_NO_WHITELIST: "s8.errRestoreNoWhitelist",
  RESTORE_TARGET_NOT_ALLOWED: "s8.errRestoreTargetNotAllowed",
  RESTORE_BAD_BACKUP_DIR: "s8.errRestoreBadBackupDir",
  // ── v0.1.20 新增 4 码（白名单由 Rust 联网复校 + 灰度开关 + 计划摘要）──
  // 全部是「整体拒绝」：什么都没改动，且**不可重试**（重试同一个备份结果一样）。
  RESTORE_REGISTRY_UNVERIFIED: "s8.errRestoreRegistryUnverified",
  RESTORE_UNSUPPORTED: "s8.errRestoreUnsupported",
  RESTORE_AUTOMATIC_PAUSED: "s8.errRestorePaused",
  RESTORE_BACKUP_TOO_OLD: "s8.errRestoreTooOld",
  RESTORE_PLAN_CHANGED: "s8.errRestorePlanChanged",
};

/**
 * 错误码 → 人话 i18n key（**不把 Rust 英文原文当说明给用户**）。
 *
 * 旧版把异常 message（英文 "failed to remove file …: permission denied (os error 5)"）
 * 直接截 64 字符当「说明」显示，却又在文案里写「看下面的说明」——小白完全看不懂。
 * 这里按机器码分流；实在认不出的再按关键字嗅（权限 / 文件被占用）。
 *
 * **还原类错误码（`RESTORE_*`）各自独立**（P2-7）：它们全是「**整体拒绝**」——
 * 什么都没改动、只能人工取回；统一落 `s8.errRemove`（「可能部分改动」）是**误导**。
 */
export function manageErrorKey(code: string, human: string): string {
  const direct = MANAGE_ERROR_KEYS[code];
  if (direct) return direct;
  const h = human.toLowerCase();
  if (/permission denied|access is denied|os error 5/.test(h)) return "s8.errPermission";
  if (/being used by another process|cannot access|os error 32/.test(h)) return "s8.errInUse";
  return "s8.errRemove";
}

/**
 * 还原流程专用：错误码 → 人话 i18n key（**fail-closed**）。
 *
 * **为什么不用 `manageErrorKey` 的兜底**：它最后落到 `s8.errRemove`
 * （「卸载失败，你的数据可能有部分变动」）—— 对还原**不成立**
 * （还原只挪走不删除；且 Rust 的 `RESTORE_*` 码全是「整体拒绝」）。
 * 未知的还原码 → 还原专用兜底文案（不承诺成功、也不谎称「什么都没变」以外的猜测）。
 */
export function restoreErrorKey(code: string): string {
  if (!code.startsWith("RESTORE_")) return "s8.errRestoreGeneric";
  return MANAGE_ERROR_KEYS[code] ?? "s8.errRestoreGeneric";
}

/**
 * 可重试的错误码**白名单**（fail-closed）。
 *
 * **为什么用白名单而不是黑名单**（独立审计 2026-11-20 NOTE-4）：
 * 后端将来新增安全类错误码而前端未同步时，黑名单会让未知码「看起来可重试」→
 * 用户反复重试一个可能破坏数据的操作。白名单下未知码一律保守处理（不给重试）。
 *
 * **`RESTORE_*` 一律不可重试**（P2-7）：它们是「整体拒绝」——重试同一个备份
 * 结果一样，只会让人以为多试几次就能成。
 */
const RETRYABLE_MANAGE_ERRORS = new Set(["MANAGE_DELETE_FAILED", "IO_ERROR"]);

/** 只有白名单里的错误才给「重试」按钮（其余按定义或按保守原则不可重试） */
export function isRetryableManageError(code: string): boolean {
  return RETRYABLE_MANAGE_ERRORS.has(code);
}
