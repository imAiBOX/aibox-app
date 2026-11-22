/**
 * 还原流程（全屏）：预览 → 破坏性确认卡 → 执行 → 结果/错误。
 *
 * **审计要求的逐条落地**（两轮独立审计的原话）：
 * ① 逐条**绝对路径**清单（默认前 20 条，>20 折叠 + 展开/收起）；
 * ② 「旧文件不会被删除，只会被挪到保全目录」（有保全目录时显示路径）；
 * ③ 「还原 ≠ 版本降级：只恢复数据文件，不会改变应用版本」；
 * ④ 勾选「我看过上面的文件清单」→ **默认不勾**，不勾不能确认；
 * ⑤ 「还原」+「取消」两个按钮。
 *
 * 另外两条硬规则：
 * - **不假装成功**：结果页逐条列出跳过/失败项（人话原因 + 技术细节折叠区），
 *   还原了几条就写几条，没做的事绝不写成做了。
 * - **`RESTORE_*` 错误不可重试**（重试同一个备份结果一样）→ 错误页只给「关闭」。
 */
import { useCallback, useEffect, useMemo, useReducer } from "react";
import { useTranslation } from "react-i18next";
import { api, type RestoreIssue } from "../net/client";
import { restoreErrorKey } from "../state/manage";
import { errorCodeOrNull } from "../state/orchestrate";
import {
  canConfirmRestore,
  createInitialRestoreState,
  formatBytes,
  restoreIssueKey,
  restoreReduce,
  visiblePlanEntries,
} from "../state/restore";
import { maskSecrets } from "../lib/redact";

interface Props {
  /** 备份目录（Rust 侧要求必须在 ~/.aibox/backup 之内） */
  backupDir: string;
  appId: string;
  onClose: () => void;
}

/** 逐条问题：人话原因 + 目标路径 + 技术细节（折叠） */
function IssueRow({ issue, tone }: { issue: RestoreIssue; tone: "skip" | "fail" }) {
  const { t } = useTranslation();
  return (
    <li className="space-y-1">
      <p className={tone === "fail" ? "text-xs text-aibox-danger" : "text-xs text-aibox-text1"}>
        {t(restoreIssueKey(issue.code))}
      </p>
      <p className="break-all font-mono text-[11px] text-aibox-text2">{issue.target}</p>
      {issue.detail && (
        <details className="font-mono text-[11px] text-aibox-text2">
          <summary className="cursor-pointer">{t("s8.techDetail")}</summary>
          <p className="mt-1 break-all">{maskSecrets(issue.detail)}</p>
        </details>
      )}
    </li>
  );
}

export function RestoreFlow({ backupDir, appId, onClose }: Props) {
  const { t } = useTranslation();
  const [state, dispatch] = useReducer(restoreReduce, undefined, createInitialRestoreState);

  /** 预览（**不写任何东西**） */
  const loadPreview = useCallback(async () => {
    dispatch({ type: "R_PREVIEW_LOADING" });
    try {
      const preview = await api.manageRestorePreview(backupDir, appId);
      dispatch({ type: "R_PREVIEW_LOADED", preview });
    } catch (e) {
      const code = errorCodeOrNull(e) ?? "";
      dispatch({ type: "R_PREVIEW_ERROR", code, messageKey: restoreErrorKey(code) });
    }
  }, [backupDir, appId]);

  useEffect(() => {
    void loadPreview();
  }, [loadPreview]);

  /** 执行还原（Rust 会重算计划并比对摘要 → 不一致则拒绝写入） */
  const run = useCallback(async () => {
    const digest = state.preview?.plan.digest;
    if (!digest || !canConfirmRestore(state)) return;
    dispatch({ type: "R_RUN" });
    try {
      const result = await api.manageRestore(backupDir, appId, digest);
      dispatch({ type: "R_DONE", result });
    } catch (e) {
      const code = errorCodeOrNull(e) ?? "";
      dispatch({ type: "R_ERROR", code, messageKey: restoreErrorKey(code) });
    }
  }, [backupDir, appId, state]);

  const plan = state.preview?.plan ?? null;
  const { shown, hidden } = useMemo(() => visiblePlanEntries(plan, 20), [plan]);
  /** 展开后显示全部条目；未展开只显示前 20 条 */
  const entries = state.expanded ? (plan?.entries ?? []) : shown;
  /** 备份时间（清单没写时间 → 破折号，不编造） */
  const planWhen = plan && plan.createdAtMs > 0 ? new Date(plan.createdAtMs).toLocaleString() : "—";

  // ---------- 预览中 ----------
  if (state.step === "previewing" || state.step === "idle" || state.step === "listing") {
    return (
      <div className="mx-auto flex h-full max-w-xl flex-col gap-4">
        <header className="flex items-center justify-between">
          <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s8.restoreConfirmTitle")}</h1>
          <button
            type="button"
            onClick={onClose}
            className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
          >
            {t("net.back")}
          </button>
        </header>
        <p className="text-sm text-aibox-text2">{t("s8.restoreWorking")}</p>
      </div>
    );
  }

  // ---------- 错误（`RESTORE_*` 一律不可重试） ----------
  if (state.step === "error") {
    const code = state.error?.code ?? "";
    // 只有「预览阶段 + 非 RESTORE_* 码」才给重试（网络抖动之类）；
    // `RESTORE_*` 是「整体拒绝」，重试同一个备份结果一样 → 不给。
    const canRetry = state.preview === null && !code.startsWith("RESTORE_");
    return (
      <div className="mx-auto flex h-full max-w-xl flex-col gap-4">
        <header className="flex items-center justify-between">
          <h1 className="font-display text-xl font-bold text-aibox-danger">{t("s8.errTitle")}</h1>
          <button
            type="button"
            onClick={onClose}
            className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
          >
            {t("net.back")}
          </button>
        </header>
        <div className="panel text-sm text-aibox-text1">
          <p>{t(state.error?.messageKey ?? "s8.errRestoreGeneric")}</p>
          {code && (
            <details className="mt-2 font-mono text-[11px] text-aibox-text2">
              <summary className="cursor-pointer">{t("s8.techDetail")}</summary>
              <p className="mt-1 break-all">{maskSecrets(code)}</p>
            </details>
          )}
        </div>
        <div className="flex gap-3">
          {canRetry && (
            <button type="button" className="btn-cta" onClick={() => void loadPreview()}>
              {t("s8.retry")}
            </button>
          )}
          <button
            type="button"
            onClick={onClose}
            className="rounded-[4px] border border-aibox-line px-4 py-2 text-sm text-aibox-cream hover:border-aibox-green"
          >
            {t("s8.close")}
          </button>
        </div>
      </div>
    );
  }

  // ---------- 执行中 ----------
  if (state.step === "running") {
    return (
      <div className="mx-auto flex h-full max-w-xl flex-col gap-4">
        <header className="flex items-center justify-between">
          <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s8.restoreConfirmTitle")}</h1>
        </header>
        <p className="text-sm text-aibox-text2">{t("s8.restoreWorking")}</p>
      </div>
    );
  }

  // ---------- 结果 ----------
  if (state.step === "done") {
    const result = state.result;
    const skipped = result?.skipped ?? [];
    const failed = result?.failed ?? [];
    return (
      <div className="mx-auto flex h-full max-w-2xl flex-col gap-4">
        <header className="flex items-center justify-between">
          <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s8.restoreDoneTitle")}</h1>
          <button
            type="button"
            onClick={onClose}
            className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
          >
            {t("net.back")}
          </button>
        </header>

        <div className="panel space-y-2 text-sm">
          <p className="text-aibox-cream">{t("s8.restoreDoneCount", { count: result?.restored ?? 0 })}</p>
          {result?.preservedDir && (
            <div className="space-y-1">
              <p className="text-aibox-text1">{t("s8.restoreDonePreserved", { dir: result.preservedDir })}</p>
              <p className="text-xs text-aibox-text2">{t("s8.restorePreserveNote", { dir: result.preservedDir })}</p>
              <button
                type="button"
                onClick={() => void api.revealDir(result.preservedDir!).catch(() => undefined)}
                className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-[10px] text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
              >
                {t("s8.restoreOpenPreserved")}
              </button>
            </div>
          )}
        </div>

        {/* 跳过 / 失败分两栏：混在一起用户会把「失败」误读成「本来就没有」 */}
        <div className="grid gap-3 sm:grid-cols-2">
          <section className="panel space-y-2">
            <p className="font-display text-sm font-semibold text-aibox-cream">
              {t("s8.restoreSkippedTitle", { count: skipped.length })}
            </p>
            {skipped.length === 0 ? (
              <p className="text-xs text-aibox-text2">—</p>
            ) : (
              <ul className="space-y-2">
                {skipped.map((it, i) => (
                  <IssueRow key={`${it.target}-${i}`} issue={it} tone="skip" />
                ))}
              </ul>
            )}
          </section>
          <section className="panel space-y-2">
            <p className="font-display text-sm font-semibold text-aibox-cream">
              {t("s8.restoreFailedTitle", { count: failed.length })}
            </p>
            {failed.length === 0 ? (
              <p className="text-xs text-aibox-text2">—</p>
            ) : (
              <ul className="space-y-2">
                {failed.map((it, i) => (
                  <IssueRow key={`${it.target}-${i}`} issue={it} tone="fail" />
                ))}
              </ul>
            )}
          </section>
        </div>

        <div className="flex gap-3">
          <button
            type="button"
            onClick={onClose}
            className="rounded-[4px] border border-aibox-line px-4 py-2 text-sm text-aibox-cream hover:border-aibox-green"
          >
            {t("s8.close")}
          </button>
        </div>
      </div>
    );
  }

  // ---------- 确认卡（破坏性操作：默认不勾选） ----------
  return (
    <div className="mx-auto flex h-full max-w-2xl flex-col gap-4">
      <header className="flex items-center justify-between">
        <div>
          <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s8.restoreConfirmTitle")}</h1>
          {/* 哪个应用（用户要确认的是「这个应用的数据」） */}
          <p className="font-mono text-xs text-aibox-text2">{appId}</p>
        </div>
        <button
          type="button"
          onClick={onClose}
          className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          {t("net.back")}
        </button>
      </header>

      <div className="panel space-y-3 border-aibox-danger/60 text-sm">
        {/* 概要：文件数 / 体积 / 备份时间 */}
        <p className="font-mono text-xs text-aibox-text2">
          {plan
            ? t("s8.restoreEntry", {
                when: planWhen,
                count: plan.fileCount,
                size: formatBytes(plan.totalBytes),
              })
            : ""}
        </p>

        {/* ① 逐条绝对路径清单（默认前 20 条；>20 折叠） */}
        <div>
          <p className="mb-1 text-xs text-aibox-text2">{t("s8.restoreWhat")}</p>
          <ul className="space-y-1">
            {entries.map((e, i) => (
              <li key={`${e.target}-${i}`} className="break-all font-mono text-xs text-aibox-text1">
                {e.target}
              </li>
            ))}
          </ul>
          {(hidden > 0 || state.expanded) && (
            <button
              type="button"
              onClick={() => dispatch({ type: "R_EXPAND", expanded: !state.expanded })}
              className="mt-2 rounded-[4px] border border-aibox-line px-3 py-1 font-mono text-[11px] text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
            >
              {state.expanded ? t("s8.restoreShowLess") : t("s8.restoreShowMore", { count: hidden })}
            </button>
          )}
        </div>

        {/* ② 旧文件不会被删除，只会被挪到保全目录 */}
        <p className="text-aibox-green">{t("s8.restoreNoDelete")}</p>
        {(plan?.wouldPreserve ?? 0) > 0 && (
          <p className="text-xs text-aibox-text2">{t("s8.restorePreserveUnknown")}</p>
        )}

        {/* ③ 还原 ≠ 版本降级 */}
        <p className="text-xs text-aibox-text2">{t("s8.restoreNotDowngrade")}</p>

        {/* ④ 默认不勾选；不勾不能确认 */}
        <label className="flex items-start gap-2">
          <input
            type="checkbox"
            className="mt-1"
            checked={state.ack}
            onChange={(e) => dispatch({ type: "R_ACK", ack: e.target.checked })}
          />
          <span className="text-aibox-text1">{t("s8.restoreAck")}</span>
        </label>

        {/* ⑤ 确认 + 取消 */}
        <div className="flex gap-3 pt-1">
          <button
            type="button"
            disabled={!canConfirmRestore(state)}
            onClick={() => void run()}
            className="rounded-[4px] bg-aibox-danger px-4 py-2 text-sm font-semibold text-white disabled:opacity-40"
          >
            {t("s8.restoreConfirmBtn")}
          </button>
          <button
            type="button"
            onClick={onClose}
            className="rounded-[4px] border border-aibox-line px-4 py-2 text-sm text-aibox-cream hover:border-aibox-green"
          >
            {t("btn.cancel")}
          </button>
        </div>
      </div>
    </div>
  );
}
