/**
 * E1 内嵌诊断面板：EXECUTE 同一任务连败 2 次 → 从右滑出
 * （不打断当前步骤上下文）：最近一次 D0-D4 摘要 + 场景化建议（人话）
 * +「导出诊断」「重试」按钮；关闭回到 S4。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, type DiagReport } from "../net/client";
import { staticRegionFallback } from "../net/region";
import { canSwitchSource } from "../state";
import { DOT_CLASS, mapDiagRows, worstStatus } from "../net/diagmap";
import DiagExport from "./DiagExport";

/** 体检超过该时长仍未返回 → 提示「体检较慢」（不阻塞面板其他内容） */
export const DOCTOR_SLOW_MS = 15_000;

export default function E1DiagPanel({
  open,
  onClose,
  onRetry,
  onRetrySource,
  errorCode,
  regionEffective,
}: {
  open: boolean;
  onClose: () => void;
  onRetry: () => void;
  /** 换源重试（跳过当前失败通道；与 ErrorOverlay 同语义） */
  onRetrySource: () => void;
  /** 当前错误码（建议文案分支：哈希/网络类不同话术） */
  errorCode?: string;
  /** 生效区域（cn → 不给国内用户「切国际」反建议；缺省前端静态判定） */
  regionEffective?: "cn" | "global" | null;
}) {
  const { t, i18n } = useTranslation();
  const [report, setReport] = useState<DiagReport | null>(null);
  const [failed, setFailed] = useState(false);
  const [slow, setSlow] = useState(false);

  useEffect(() => {
    if (!open) return;
    let alive = true;
    api
      .netdoctorRun(["D0", "D1", "D2", "D3", "D4"])
      .then((r) => alive && setReport(r))
      .catch(() => alive && setFailed(true));
    return () => {
      alive = false;
    };
  }, [open]);

  // 体检进度语义：>15s 未返回 → 「体检较慢」（结果到达后自然替换）
  useEffect(() => {
    if (!open || report || failed) {
      setSlow(false);
      return;
    }
    const timer = setTimeout(() => setSlow(true), DOCTOR_SLOW_MS);
    return () => clearTimeout(timer);
  }, [open, report, failed]);

  const rows = mapDiagRows(report, i18n.resolvedLanguage ?? "en");
  const worst = worstStatus(rows);

  // 建议分支：真篡改 → 换源说明；网络类 → 按生效区域（cn 序不劝「切国际」）
  const effective = regionEffective ?? staticRegionFallback();
  const adviceKey =
    errorCode === "NET_HASH_MISMATCH" || errorCode === "HASH_MISMATCH"
      ? "e1.adviceHashMismatch"
      : effective === "global"
        ? "e1.r2Advice"
        : "e1.r2AdviceCn";
  const switchable = errorCode != null && canSwitchSource(errorCode);

  return (
    <aside
      data-testid="e1-panel"
      aria-hidden={!open}
      className={[
        "fixed right-0 top-0 z-40 flex h-full w-full max-w-sm flex-col border-l border-aibox-green bg-aibox-raised p-5 shadow-2xl transition-transform duration-300",
        open ? "translate-x-0" : "translate-x-full",
      ].join(" ")}
    >
      <header className="mb-3 flex items-center justify-between">
        <h2 className="font-display text-base font-semibold text-aibox-cream">{t("e1.title")}</h2>
        <button
          type="button"
          onClick={onClose}
          aria-label={t("e1.close")}
          className="rounded-[4px] border border-aibox-line px-2.5 py-1 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          ✕
        </button>
      </header>

      <p className="mb-3 text-sm leading-relaxed text-aibox-text2">{t("e1.desc")}</p>

      {/* D0-D4 摘要 */}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {failed && (
          <p className="font-mono text-[11px] text-aibox-danger">{t("net.doctorFail")}</p>
        )}
        {!failed && rows.length === 0 && (
          <p className="font-mono text-[11px] text-aibox-text2">
            {slow ? t("net.doctorSlow") : t("net.doctorRunning")}
          </p>
        )}
        {rows.length > 0 && (
          <ul className="flex flex-col gap-1.5">
            {rows.map((r) => (
              <li
                key={r.key}
                className="flex items-center gap-2 rounded-[4px] border border-aibox-line bg-aibox-bg px-2.5 py-1.5"
              >
                <span
                  className={`h-2.5 w-2.5 shrink-0 rounded-[2px] ${DOT_CLASS[r.dot]}`}
                  aria-label={t(`net.status.${r.dot}`)}
                />
                <span className="font-mono text-[11px] text-aibox-text2">{r.item}</span>
                <span className="min-w-0 flex-1 text-xs text-aibox-text1">{r.summary}</span>
                {r.fixes.includes("R2") && (
                  <span className="rounded-[3px] border border-aibox-info/60 px-1.5 py-0.5 font-mono text-[10px] text-aibox-info">
                    R2
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}

        {/* R2 建议（人话） */}
        <div className="mt-3 rounded-[4px] border border-aibox-info/40 bg-aibox-info/10 p-3">
          <p className="mb-1 font-mono text-[10px] uppercase tracking-wider text-aibox-info">
            {t("e1.r2Tag")}
          </p>
          <p className="text-xs leading-relaxed text-aibox-text1">{t(adviceKey)}</p>
          {worst === "fail" && (
            <p className="mt-1.5 text-xs leading-relaxed text-aibox-text2">{t("e1.failHint")}</p>
          )}
        </div>
      </div>

      {/* 操作：换源 / 导出诊断 / 重试 */}
      <div className="mt-4 flex flex-wrap items-center justify-between gap-2 border-t border-aibox-line pt-4">
        <DiagExport compact symptom={t("e1.symptom")} />
        <div className="flex gap-2">
          {switchable && (
            <button
              type="button"
              onClick={onRetrySource}
              data-testid="e1-switch-source"
              className="rounded-[4px] border border-aibox-green px-3 py-2 font-mono text-xs text-aibox-green hover:bg-aibox-green/10"
            >
              {t("err.switchSource")}
            </button>
          )}
          <button
            type="button"
            onClick={onRetry}
            className="rounded-[4px] bg-aibox-green px-4 py-2 font-display text-sm font-semibold text-aibox-bg"
          >
            {t("e1.retry")}
          </button>
        </div>
      </div>
    </aside>
  );
}
