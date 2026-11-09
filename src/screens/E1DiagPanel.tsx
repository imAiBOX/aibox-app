/**
 * E1 内嵌诊断面板：EXECUTE 同一任务连败 2 次 → 从右滑出
 * （不打断当前步骤上下文）：最近一次 D0-D4 摘要 + R2 建议（人话）
 * +「导出诊断」「重试」按钮；关闭回到 S4。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, type DiagReport } from "../net/client";
import { DOT_CLASS, mapDiagRows, worstStatus } from "../net/diagmap";
import DiagExport from "./DiagExport";

export default function E1DiagPanel({
  open,
  onClose,
  onRetry,
}: {
  open: boolean;
  onClose: () => void;
  onRetry: () => void;
}) {
  const { t, i18n } = useTranslation();
  const [report, setReport] = useState<DiagReport | null>(null);
  const [failed, setFailed] = useState(false);

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

  const rows = mapDiagRows(report, i18n.resolvedLanguage ?? "en");
  const worst = worstStatus(rows);

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
          <p className="font-mono text-[11px] text-aibox-text2">{t("net.doctorRunning")}</p>
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
          <p className="text-xs leading-relaxed text-aibox-text1">{t("e1.r2Advice")}</p>
          {worst === "fail" && (
            <p className="mt-1.5 text-xs leading-relaxed text-aibox-text2">{t("e1.failHint")}</p>
          )}
        </div>
      </div>

      {/* 操作：导出诊断 / 重试 */}
      <div className="mt-4 flex items-center justify-between gap-2 border-t border-aibox-line pt-4">
        <DiagExport compact symptom={t("e1.symptom")} />
        <button
          type="button"
          onClick={onRetry}
          className="shrink-0 rounded-[4px] bg-aibox-green px-4 py-2 font-display text-sm font-semibold text-aibox-bg"
        >
          {t("e1.retry")}
        </button>
      </div>
    </aside>
  );
}
