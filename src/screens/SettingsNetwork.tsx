/**
 * 设置-网络页 E2：地区选择（net_region_set 真接线，**唯一有效的下载网络开关**）/
 * 「立即体检」→ netdoctor_run → 体检卡列表（三色方块 + 按语言 summary + R 徽章 + detail 展开）
 * + 导出诊断（DiagExport 共用控件）。
 *
 * **已下线**：原「使用系统代理」开关 +「自定义下载前缀」。
 * 这两个开关只写进前端本地 settings，**下载层根本没人读**（通道来自 registry 的 channels），
 * 开了和没开完全一样。而 E1 的失败建议却让用户「去打开代理开关」——把用户引向无效操作。
 * 真要接上代理/自定义源，需后端新增命令并让下载层消费设置；在那之前不摆这个开关。
 */
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, type DiagReport } from "../net/client";
import { DOT_CLASS, mapDiagRows, worstStatus } from "../net/diagmap";
import DiagExport from "./DiagExport";

/** detail 是 JSON 字符串（Rust 侧 stringify 下发）→ 美化展示；非 JSON 原样返回 */
function fmtDetail(detail: string): string {
  try {
    return JSON.stringify(JSON.parse(detail), null, 2);
  } catch {
    return detail;
  }
}

type Region = "auto" | "cn" | "global";

export default function SettingsNetwork() {
  const { t, i18n } = useTranslation();
  const [region, setRegion] = useState<Region>("auto");
  const [report, setReport] = useState<DiagReport | null>(null);
  const [doctorBusy, setDoctorBusy] = useState(false);
  const [doctorErr, setDoctorErr] = useState<string | null>(null);

  useEffect(() => {
    void api
      .netRegion()
      .then((r) => setRegion(r.source as Region))
      .catch(() => /* mock/Tauri 未就绪 → 保持默认 */ undefined);
  }, []);

  const saveRegion = (v: Region) => {
    setRegion(v);
    void api.netRegionSet(v).catch(() => undefined);
  };

  const runDoctor = useCallback(() => {
    setDoctorBusy(true);
    setDoctorErr(null);
    api
      .netdoctorRun()
      .then(setReport)
      .catch(() => setDoctorErr(t("net.doctorFail")))
      .finally(() => setDoctorBusy(false));
  }, [t]);

  const rows = mapDiagRows(report, i18n.resolvedLanguage ?? "en");
  const worst = worstStatus(rows);

  return (
    <div className="flex min-h-0 flex-1 flex-col gap-5 overflow-y-auto pr-1">
      {/* 地区选择（真 net_region_set） */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <h2 className="mb-2 font-display text-sm font-semibold text-aibox-cream">
          {t("net.regionTitle")}
        </h2>
        <div className="flex gap-2">
          {(["auto", "cn", "global"] as const).map((r) => (
            <button
              key={r}
              type="button"
              onClick={() => saveRegion(r)}
              className={[
                "rounded-[4px] border px-3 py-1.5 font-mono text-xs",
                region === r
                  ? "border-aibox-green text-aibox-green"
                  : "border-aibox-line text-aibox-text2",
              ].join(" ")}
            >
              {t(`net.region.${r}`)}
            </button>
          ))}
        </div>
        <p className="mt-2 font-mono text-[11px] text-aibox-text2">{t("net.regionHint")}</p>
      </section>

      {/* 立即体检（E2：netdoctor_run 真报告） */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <div className="flex items-center justify-between gap-3">
          <div>
            <h2 className="font-display text-sm font-semibold text-aibox-cream">
              {t("net.doctorTitle")}
            </h2>
            <p className="text-xs text-aibox-text2">{t("net.doctorDesc")}</p>
          </div>
          <button
            type="button"
            onClick={runDoctor}
            disabled={doctorBusy}
            className="shrink-0 rounded-[4px] border border-aibox-green px-3 py-2 font-mono text-xs text-aibox-green hover:bg-aibox-green hover:text-aibox-bg disabled:opacity-50"
          >
            {doctorBusy ? t("net.doctorRunning") : t("net.doctorRun")}
          </button>
        </div>

        {doctorErr && (
          <p className="mt-2 font-mono text-[11px] text-aibox-danger" role="alert">
            {doctorErr}
          </p>
        )}

        {rows.length > 0 && (
          <ul className="mt-3 flex flex-col gap-2" data-testid="diag-list">
            {rows.map((r) => (
              <li key={r.key} className="rounded-[4px] border border-aibox-line bg-aibox-bg p-2.5">
                <div className="flex items-center gap-2">
                  <span
                    className={`h-2.5 w-2.5 shrink-0 rounded-[2px] ${DOT_CLASS[r.dot]}`}
                    aria-label={t(`net.status.${r.dot}`)}
                  />
                  <span className="font-mono text-[11px] text-aibox-text2">{r.item}</span>
                  <span className="min-w-0 flex-1 text-xs text-aibox-text1">{r.summary}</span>
                  {r.fixes.map((f) => (
                    <span
                      key={f}
                      title={t(`net.fixHint.${f}`)}
                      className="rounded-[3px] border border-aibox-info/60 px-1.5 py-0.5 font-mono text-[10px] text-aibox-info"
                    >
                      {t(`net.fixBadge.${f}`)}
                    </span>
                  ))}
                </div>
                {r.detail && (
                  <details className="mt-1.5">
                    <summary className="cursor-pointer font-mono text-[10px] text-aibox-text2">
                      {t("net.detailToggle")}
                    </summary>
                    <p className="mt-1 break-all whitespace-pre-wrap font-mono text-[10px] leading-relaxed text-aibox-text2">
                      {fmtDetail(r.detail)}
                    </p>
                  </details>
                )}
              </li>
            ))}
            {worst && (
              <li className="font-mono text-[11px] text-aibox-text2">
                {t("net.doctorDone")}: {t(`net.status.${worst}`)}
              </li>
            )}
          </ul>
        )}
      </section>

      {/* 导出诊断（与 ErrorOverlay 共用控件） */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <h2 className="mb-1 font-display text-sm font-semibold text-aibox-cream">
          {t("net.diagTitle")}
        </h2>
        <p className="mb-2.5 text-xs text-aibox-text2">{t("net.diagDesc")}</p>
        <DiagExport symptom={t("net.diagSymptom")} />
      </section>
    </div>
  );
}
