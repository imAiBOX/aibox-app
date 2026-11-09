/**
 * 设置-网络页 E2 骨架：地区选择 / PROXY 开关 / CUSTOM 前缀 /
 * 「立即体检」按钮（占位，接 netdoctor_run）。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../net/client";

type Region = "auto" | "cn" | "global";

export default function SettingsNetwork({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const [region, setRegion] = useState<Region>("auto");
  const [proxyOn, setProxyOn] = useState(false);
  const [proxyAddr, setProxyAddr] = useState("http://127.0.0.1:7890");
  const [customOn, setCustomOn] = useState(false);
  const [customPrefix, setCustomPrefix] = useState("https://ghfast.top/");
  const [doctorNote, setDoctorNote] = useState<string | null>(null);

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

  return (
    <div className="mx-auto flex h-full max-w-xl flex-col gap-5">
      <header className="flex items-center justify-between">
        <h1 className="font-display text-xl font-bold text-aibox-cream">{t("net.title")}</h1>
        <button
          type="button"
          onClick={onClose}
          className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          {t("net.back")}
        </button>
      </header>

      {/* 地区选择 */}
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

      {/* PROXY 开关 */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <div className="flex items-center justify-between">
          <h2 className="font-display text-sm font-semibold text-aibox-cream">{t("net.proxyTitle")}</h2>
          <button
            type="button"
            role="switch"
            aria-checked={proxyOn}
            onClick={() => setProxyOn((v) => !v)}
            className={[
              "h-6 w-11 rounded-full border transition-colors",
              proxyOn ? "border-aibox-green bg-aibox-green/30" : "border-aibox-line bg-aibox-bg",
            ].join(" ")}
          >
            <span
              className={[
                "block h-4 w-4 rounded-full transition-transform",
                proxyOn ? "translate-x-6 bg-aibox-green" : "translate-x-1 bg-aibox-text2",
              ].join(" ")}
            />
          </button>
        </div>
        {proxyOn && (
          <input
            type="text"
            value={proxyAddr}
            onChange={(e) => setProxyAddr(e.target.value)}
            className="mt-3 w-full rounded-[4px] border border-aibox-line bg-aibox-bg px-3 py-2 font-mono text-sm text-aibox-text1 outline-none focus:border-aibox-green"
            aria-label={t("net.proxyAddr")}
          />
        )}
      </section>

      {/* CUSTOM 前缀 */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <div className="flex items-center justify-between">
          <h2 className="font-display text-sm font-semibold text-aibox-cream">{t("net.customTitle")}</h2>
          <button
            type="button"
            role="switch"
            aria-checked={customOn}
            onClick={() => setCustomOn((v) => !v)}
            className={[
              "h-6 w-11 rounded-full border transition-colors",
              customOn ? "border-aibox-green bg-aibox-green/30" : "border-aibox-line bg-aibox-bg",
            ].join(" ")}
          >
            <span
              className={[
                "block h-4 w-4 rounded-full transition-transform",
                customOn ? "translate-x-6 bg-aibox-green" : "translate-x-1 bg-aibox-text2",
              ].join(" ")}
            />
          </button>
        </div>
        {customOn && (
          <input
            type="text"
            value={customPrefix}
            onChange={(e) => setCustomPrefix(e.target.value)}
            className="mt-3 w-full rounded-[4px] border border-aibox-line bg-aibox-bg px-3 py-2 font-mono text-sm text-aibox-text1 outline-none focus:border-aibox-green"
            aria-label={t("net.customPrefix")}
          />
        )}
      </section>

      {/* 立即体检（占位：接 netdoctor_run D0-D4 报告） */}
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
            onClick={() => setDoctorNote(t("net.doctorPending"))}
            className="shrink-0 rounded-[4px] border border-aibox-green px-3 py-2 font-mono text-xs text-aibox-green hover:bg-aibox-green hover:text-aibox-bg"
          >
            {t("net.doctorRun")}
          </button>
        </div>
        {doctorNote && (
          <p className="mt-2 font-mono text-[11px] text-aibox-info">{doctorNote}</p>
        )}
      </section>
    </div>
  );
}
