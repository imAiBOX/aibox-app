/**
 * 设置-通用：开机自启动开关（即时生效，失败人话提示）+ 托盘行为说明。
 * 自启 = tauri-plugin-autostart（Windows 写 HKCU Run 键，免提权；默认开）。
 */
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, type AutostartState } from "../net/client";

export default function SettingsGeneral() {
  const { t } = useTranslation();
  const [st, setSt] = useState<AutostartState | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void api
      .autostartGet()
      .then(setSt)
      .catch(() => setSt({ enabled: false }));
  }, []);

  const toggle = useCallback(() => {
    if (!st || busy) return;
    const next = !st.enabled;
    setBusy(true);
    api
      .autostartSet(next)
      .then(setSt)
      .catch(() => setSt({ enabled: st.enabled, error: "autostart set failed" }))
      .finally(() => setBusy(false));
  }, [st, busy]);

  return (
    <>
      {/* 开机自启动（即时生效） */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <div className="flex items-center justify-between">
          <div>
            <h2 className="mb-1 font-display text-sm font-semibold text-aibox-cream">
              {t("gen.autostart")}
            </h2>
            <p className="text-xs text-aibox-text2">{t("gen.autostartDesc")}</p>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={st?.enabled ?? false}
            disabled={st === null || busy}
            onClick={toggle}
            className={[
              "h-6 w-11 shrink-0 rounded-full border transition-colors disabled:opacity-50",
              st?.enabled ? "border-aibox-green bg-aibox-green/30" : "border-aibox-line bg-aibox-bg",
            ].join(" ")}
          >
            <span
              className={[
                "block h-4 w-4 rounded-full transition-transform",
                st?.enabled ? "translate-x-6 bg-aibox-green" : "translate-x-1 bg-aibox-text2",
              ].join(" ")}
            />
          </button>
        </div>
        {st?.error && (
          <p
            className="mt-2 font-mono text-[11px] text-aibox-danger"
            role="alert"
            title={st.error}
          >
            {t("gen.autostartFail")}
          </p>
        )}
      </section>

      {/* 托盘行为说明（一行人话） */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <h2 className="mb-1 font-display text-sm font-semibold text-aibox-cream">
          {t("gen.trayTitle")}
        </h2>
        <p className="text-xs text-aibox-text2">{t("gen.trayHint")}</p>
      </section>
    </>
  );
}
