import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { setLang, type Lang } from "./i18n";

/** 像素方块母题：8×8 机器人脸（1=主绿点亮，0=暗格），母题 */
const FACE = [
  "00111100",
  "01111110",
  "11011011",
  "11111111",
  "10111101",
  "11000011",
  "01111110",
  "00100100",
];

export default function App() {
  const { t, i18n } = useTranslation();
  const [version, setVersion] = useState(__APP_VERSION__);

  // 版本号优先取 Tauri 壳（与 tauri.conf.json 同源），失败回落构建期注入值
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => {
        /* 非 Tauri 环境（纯 vite dev）→ 保留 __APP_VERSION__ */
      });
  }, []);

  const isZh = (i18n.resolvedLanguage ?? i18n.language ?? "en").startsWith("zh");
  const toggleLang = () => setLang((isZh ? "en" : "zh-CN") as Lang);

  return (
    <div className="relative flex min-h-screen flex-col items-center justify-center gap-6 bg-aibox-bg px-6 text-center">
      <header className="absolute inset-x-0 top-0 flex items-center justify-between px-5 py-4">
        <span className="font-display text-sm font-bold tracking-[0.3em] text-aibox-cream">
          AiBOX
        </span>
        <button
          type="button"
          onClick={toggleLang}
          className="rounded-[4px] border border-aibox-line bg-aibox-raised px-3 py-1.5 font-mono text-xs text-aibox-text2 transition-colors hover:border-aibox-green hover:text-aibox-green"
          aria-label="switch language"
        >
          {t("app.langSwitch")}
        </button>
      </header>

      <main className="flex flex-col items-center gap-6">
        {/* 像素方块母题（loading 方块逐个点亮的静态版） */}
        <div className="grid grid-cols-8 gap-1.5" aria-hidden="true">
          {FACE.flatMap((row, r) =>
            row.split("").map((cell, c) => (
              <span
                key={`${r}-${c}`}
                className={[
                  "h-3.5 w-3.5 rounded-[3px]",
                  cell === "1"
                    ? "bg-aibox-green animate-pulse"
                    : "bg-aibox-line",
                ].join(" ")}
              />
            )),
          )}
        </div>

        <h1 className="font-display text-6xl font-bold tracking-tight text-aibox-cream sm:text-7xl">
          AiBOX
        </h1>

        <p className="font-display text-lg font-medium text-aibox-green sm:text-xl">
          {t("app.tagline")}
        </p>

        <p className="max-w-md text-sm leading-relaxed text-aibox-text2">
          {t("app.phaseNote")}
        </p>

        <div className="flex flex-wrap items-center justify-center gap-2 font-mono text-[11px] text-aibox-text2">
          <span className="rounded-[4px] border border-aibox-line bg-aibox-raised px-2 py-1">
            v{version}
          </span>
          <span className="rounded-[4px] border border-aibox-line px-2 py-1">
            {t("app.phase")}
          </span>
          <span className="rounded-[4px] border border-aibox-line px-2 py-1">
            {t("app.portable")}
          </span>
          <span className="rounded-[4px] border border-aibox-line px-2 py-1">
            {t("app.platform")}
          </span>
        </div>
      </main>

      <footer className="absolute inset-x-0 bottom-0 flex items-center justify-between px-5 py-4 font-mono text-[11px] text-aibox-text2">
        <span>{t("app.dataSource")}</span>
        <span className="text-aibox-green-d">AiBOX.im</span>
      </footer>
    </div>
  );
}
