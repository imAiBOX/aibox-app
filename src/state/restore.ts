/**
 * 安全数据还原 · 前端状态机（纯 reducer；只管状态，不碰任何 IO）。
 *
 * **为什么单独一个状态组**：还原是破坏性操作，流程与「安装八态 / MANAGE」都不同
 * （预览 → 确认 → 执行 → 结果），混在一起会让人在别处误触发。
 *
 * 硬规则（reducer 层强制，组件层只是显示）：
 * ① **默认不勾选**：新预览载入时 `ack` 一律回到 false（不勾不能执行）；
 * ② **未勾不能跑**：`R_RUN` 在 `ack === false` 时被忽略（幂等，不抛错）；
 * ③ **预览 = 执行**：Rust 侧预览与执行共用同一份判定代码，前端只负责把同一份
 *    计划原样显示（不自行增删条目）；
 * ④ 不可重试的错误（`RESTORE_*`）由 `manage.ts` 的 `manageErrorKey` 负责映射，
 *    本文件只搬运 `messageKey`。
 */
import type {
  BackupRecord,
  ManageRestoreResult,
  PlannedEntry,
  RestorePlan,
  RestorePreview,
} from "../net/client";

/** 还原流程的步骤（列表态与流程态共用一个状态组） */
export type RestoreStep =
  | "idle"
  | "listing"
  | "listError"
  | "previewing"
  | "preview"
  | "running"
  | "done"
  | "error";

export interface RestoreState {
  step: RestoreStep;
  /** 备份库条目（列表页） */
  backups: BackupRecord[];
  /** 还原预览（预览 = 执行的同一份判定结果） */
  preview: RestorePreview | null;
  /** 执行结果 */
  result: ManageRestoreResult | null;
  /** 错误（code = Rust 机器码；messageKey = 人话 i18n key） */
  error: { code: string; messageKey: string } | null;
  /** 用户已勾「我看过清单」——**默认 false**（不勾不能执行） */
  ack: boolean;
  /** 是否展开全部路径（>20 条时默认折叠） */
  expanded: boolean;
}

export type RestoreEvent =
  /** 拉备份列表中 */
  | { type: "R_LIST_LOADING" }
  /** 备份列表到货 */
  | { type: "R_LIST_LOADED"; backups: BackupRecord[] }
  /** 备份列表拉失败（可见错误 + 重试按钮） */
  | { type: "R_LIST_ERROR" }
  /** 预览中（**不写任何东西**） */
  | { type: "R_PREVIEW_LOADING" }
  /** 预览到货 → 进入确认卡（ack 一律重置为 false） */
  | { type: "R_PREVIEW_LOADED"; preview: RestorePreview }
  /** 预览失败 */
  | { type: "R_PREVIEW_ERROR"; code: string; messageKey: string }
  /** 勾选/取消「我看过清单」 */
  | { type: "R_ACK"; ack: boolean }
  /** 展开/收起全部路径 */
  | { type: "R_EXPAND"; expanded: boolean }
  /** 执行还原（守卫不过 → 忽略） */
  | { type: "R_RUN" }
  /** 执行完成 */
  | { type: "R_DONE"; result: ManageRestoreResult }
  /** 执行失败 */
  | { type: "R_ERROR"; code: string; messageKey: string }
  /** 关闭流程 → 回到初始态 */
  | { type: "R_CLOSE" };

export function createInitialRestoreState(): RestoreState {
  return {
    step: "idle",
    backups: [],
    preview: null,
    result: null,
    error: null,
    ack: false,
    expanded: false,
  };
}

export function restoreReduce(state: RestoreState, event: RestoreEvent): RestoreState {
  switch (event.type) {
    case "R_LIST_LOADING":
      // 重新拉列表：清掉旧错误，但**保留**已拉到的数据（避免闪空白）
      return { ...state, step: "listing", error: null };

    case "R_LIST_LOADED":
      return { ...state, step: "idle", backups: event.backups, error: null };

    case "R_LIST_ERROR":
      return { ...state, step: "listError", error: null };

    case "R_PREVIEW_LOADING":
      // 新预览开始：旧预览/旧结果/旧勾选全部作废（防止拿旧清单确认新备份）
      return { ...state, step: "previewing", preview: null, result: null, error: null, ack: false, expanded: false };

    case "R_PREVIEW_LOADED":
      // 硬规则①：新预览一律**不勾选**
      return { ...state, step: "preview", preview: event.preview, result: null, error: null, ack: false, expanded: false };

    case "R_PREVIEW_ERROR":
      return { ...state, step: "error", error: { code: event.code, messageKey: event.messageKey }, ack: false };

    case "R_ACK":
      // 只在确认卡态可改勾选（其他态忽略 → 幂等）
      if (state.step !== "preview") return state;
      return { ...state, ack: event.ack };

    case "R_EXPAND":
      if (state.step !== "preview") return state;
      return { ...state, expanded: event.expanded };

    case "R_RUN":
      // 硬规则②：未勾「我看过清单」→ 拒绝执行（忽略事件，原态不变）
      if (!canConfirmRestore(state)) return state;
      return { ...state, step: "running", error: null };

    case "R_DONE":
      return { ...state, step: "done", result: event.result, error: null };

    case "R_ERROR":
      return { ...state, step: "error", error: { code: event.code, messageKey: event.messageKey } };

    case "R_CLOSE":
      return createInitialRestoreState();

    default: {
      // 穷尽性检查：事件类型必须全部处理
      const _never: never = event;
      void _never;
      return state;
    }
  }
}

/**
 * 计划条目的可见切片（默认前 20 条；其余折叠）。
 *
 * **为什么要有这个**：备份可能有几百上千个文件，全列出来会把确认卡撑爆，
 * 用户反而看不到「到底要动哪些路径」。折叠 + 可展开 = 既不失控也不隐瞒。
 */
export function visiblePlanEntries(
  plan: RestorePlan | null | undefined,
  limit = 20,
): { shown: PlannedEntry[]; hidden: number } {
  const entries = plan?.entries ?? [];
  const cap = Number.isFinite(limit) && limit > 0 ? Math.floor(limit) : 20;
  if (entries.length <= cap) return { shown: [...entries], hidden: 0 };
  return { shown: entries.slice(0, cap), hidden: entries.length - cap };
}

/** 确认卡能否执行（纯守卫）：必须在预览态且已勾「我看过清单」。 */
export function canConfirmRestore(state: RestoreState): boolean {
  return state.step === "preview" && state.ack;
}

/**
 * 逐条原因码 → 人话 i18n key（9 个固定枚举；未知码走通用兜底）。
 *
 * **为什么未知码要有兜底**：Rust 侧将来加新码而前端没跟上时，
 * 不能把机器码直接甩给小白（`symlink` 这种词没人看得懂），
 * 也不能崩（t() 缺 key 会显示原始 key）。
 */
export function restoreIssueKey(code: string): string {
  const byCode: Record<string, string> = {
    symlink: "s8.restoreReasonSymlink",
    sha_mismatch: "s8.restoreReasonSha",
    type_conflict: "s8.restoreReasonType",
    outside_backup: "s8.restoreReasonOutside",
    source_missing: "s8.restoreReasonMissing",
    preserve_failed: "s8.restoreReasonPreserve",
    write_failed: "s8.restoreReasonWrite",
    verify_failed: "s8.restoreReasonVerify",
    parent_conflict: "s8.restoreReasonParent",
  };
  return byCode[code] ?? "s8.restoreReasonUnknown";
}

/** 字节数 → 人话大小（KB/MB/GB；纯函数，单测） */
export function formatBytes(bytes: number): string {
  const n = Number.isFinite(bytes) && bytes > 0 ? bytes : 0;
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}
