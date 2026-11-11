/**
 * ERROR 覆盖层（失败态 +）：红色横幅 + 人话原因 + 三按钮
 * （重试 / 跳过或换源 / 导出诊断）。ERROR 是覆盖层，不占主流程屏幕。
 * 导出走 DiagExport 共用控件：loading → zip 路径 + 一键新建 GitHub Issue。
 */
import { useTranslation } from "react-i18next";
import { canSwitchSource, errorActions, type MachineState } from "../state";
import DiagExport from "./DiagExport";

export default function ErrorOverlay({
  state,
  onRetry,
  onRetrySource,
  onSkip,
  onExported,
  onDismiss,
}: {
  state: MachineState;
  onRetry: () => void;
  /** 换源重试：跳过当前失败通道重发 download_start（与「重试」并列） */
  onRetrySource: () => void;
  onSkip: () => void;
  /** 诊断包导出成功（zip 路径）；调用方据此 dispatch EXPORT_DIAG */
  onExported: (zipPath: string) => void;
  onDismiss: () => void;
}) {
  const { t, i18n } = useTranslation();
  const err = state.error;
  if (!err) return null;
  const acts = errorActions(state);
  // 只有下载类错误才有「另一个源」可换（扫描/配置类错误不展示）
  const switchable = canSwitchSource(err.code);

  return (
    <div
      role="alertdialog"
      aria-modal="true"
      aria-labelledby="aibox-err-title"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 px-4"
    >
      <div className="w-full max-w-md rounded-[6px] border border-aibox-danger bg-aibox-raised p-5 shadow-2xl">
        <div className="mb-2 flex items-center gap-2">
          <span className="flex h-6 w-6 items-center justify-center rounded-[4px] bg-aibox-danger font-display text-sm font-bold text-aibox-bg">
            !
          </span>
          <h2
            id="aibox-err-title"
            className="font-display text-base font-semibold text-aibox-text1"
          >
            {t("err.title")}
          </h2>
        </div>

        {/* 人话原因 */}
        <p className="mb-1.5 text-sm leading-relaxed text-aibox-text1">{t(err.messageKey)}</p>
        <p className="mb-4 font-mono text-[11px] text-aibox-text2">
          {t("err.code")}: {err.code}
          {err.taskId ? ` · task=${err.taskId}` : ""}
        </p>

        {/* 三按钮：主（重试）右对齐；危险/次要左对齐 */}
        <div className="flex items-end justify-between gap-3">
          <div className="flex flex-col gap-2">
            {acts.skip && (
              <button
                type="button"
                onClick={onSkip}
                className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
              >
                {t("err.skip")}
              </button>
            )}
            {/* 导出诊断：loading → zip 路径 + 一键建 Issue */}
            <DiagExport
              compact
              lang={i18n.resolvedLanguage}
              errorCode={err.code}
              symptom={t(err.messageKey)}
              onExported={onExported}
            />
          </div>
          <div className="flex gap-2">
            <button
              type="button"
              onClick={onDismiss}
              className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2"
            >
              {t("err.close")}
            </button>
            {switchable && (
              <button
                type="button"
                onClick={onRetrySource}
                data-testid="err-switch-source"
                className="rounded-[4px] border border-aibox-green px-3 py-1.5 font-mono text-xs text-aibox-green hover:bg-aibox-green/10"
              >
                {t("err.switchSource")}
              </button>
            )}
            <button
              type="button"
              onClick={onRetry}
              disabled={!acts.retry}
              className="rounded-[4px] bg-aibox-green px-4 py-1.5 font-display text-sm font-semibold text-aibox-bg disabled:opacity-40"
            >
              {t("err.retry")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
