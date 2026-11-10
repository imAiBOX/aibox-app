/**
 * S2 SCAN 体检报告：左列系统指标表（绿√/红!），右列「将要安装」预告卡。
 * 红灯项点击展开人话解释。
 */
import { Fragment, useState } from "react";
import { useTranslation } from "react-i18next";
import { scanRows, type Plan, type ScanReport } from "../state";

export default function S2Scan({ report, plan }: { report: ScanReport | null; plan: Plan | null }) {
  const { t } = useTranslation();
  const [expanded, setExpanded] = useState<string | null>(null);

  if (!report) {
    return <p className="font-mono text-xs text-aibox-text2">{t("s2.running")}</p>;
  }
  const rows = scanRows(report);

  return (
    <div className="grid h-full gap-6 lg:grid-cols-[1fr_320px]">
      <section>
        <h1 className="mb-4 font-display text-xl font-bold text-aibox-cream">{t("s2.title")}</h1>
        <table className="w-full border-collapse text-sm">
          <tbody>
            {rows.map((it) => {
              // warn（非红非绿）用琥珀色；fail 红；ok 绿
              const mark = it.ok ? "√" : "!";
              const cls = it.ok
                ? "text-aibox-ok"
                : it.level === "warn"
                  ? "text-[#f5c518]"
                  : "text-aibox-danger";
              const expandable = !it.ok && (!!it.hintKey || !!it.noteKey);
              return (
                <Fragment key={it.id}>
                  <tr
                    className={["border-b border-aibox-line", expandable ? "cursor-pointer" : ""].join(" ")}
                    onClick={() => {
                      if (expandable) setExpanded(expanded === it.id ? null : it.id);
                    }}
                  >
                    <td className="py-2.5 pr-3 text-aibox-text2">{t(it.labelKey)}</td>
                    <td className="py-2.5 pr-3 font-mono text-xs text-aibox-text1">{it.value ?? "—"}</td>
                    <td className="w-8 py-2.5 text-right">
                      <span className={cls}>{mark}</span>
                    </td>
                    <td className="w-16 py-2.5 text-right font-mono text-[10px] text-aibox-info">
                      {expandable ? t("s2.expand") : ""}
                    </td>
                  </tr>
                  {expanded === it.id && expandable && (
                    <tr className="border-b border-aibox-line">
                      <td colSpan={4} className="bg-aibox-raised px-3 py-3 text-xs leading-relaxed text-aibox-text2">
                        {it.noteKey ? `${t(it.noteKey)} ` : ""}
                        {it.hintKey ? t(it.hintKey) : ""}
                      </td>
                    </tr>
                  )}
                </Fragment>
              );
            })}
          </tbody>
        </table>
      </section>

      <aside className="flex flex-col gap-3">
        <h2 className="font-display text-sm font-semibold text-aibox-cream">{t("s2.previewTitle")}</h2>
        <div className="flex flex-col gap-2 rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
          {(plan?.tasks ?? []).map((task) => (
            <div key={task.id} className="flex items-center justify-between font-mono text-xs">
              <span className="text-aibox-text1">{task.name}</span>
              <span className="text-aibox-text2">{task.version}</span>
            </div>
          ))}
          {!plan && <span className="font-mono text-[11px] text-aibox-text2">{t("s2.planPending")}</span>}
        </div>
        <p className="text-xs leading-relaxed text-aibox-text2">{t("s2.note")}</p>
      </aside>
    </div>
  );
}
