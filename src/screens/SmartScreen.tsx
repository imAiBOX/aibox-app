/**
 * SmartScreen 引导页骨架（.3 信任细节）：
 * 「Windows 已保护你的电脑」图文分步引导。文案 key 化，图片位占位。
 */
import { useTranslation } from "react-i18next";

export default function SmartScreen({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const steps = ["ss.step1", "ss.step2", "ss.step3", "ss.step4"];

  return (
    <div className="mx-auto flex h-full max-w-xl flex-col gap-5">
      <header className="flex items-center justify-between">
        <h1 className="font-display text-xl font-bold text-aibox-cream">{t("ss.title")}</h1>
        <button
          type="button"
          onClick={onClose}
          className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          {t("ss.back")}
        </button>
      </header>

      <p className="text-sm leading-relaxed text-aibox-text2">{t("ss.intro")}</p>

      {/* 竖向时间线（3 步母题，同款） */}
      <ol className="flex flex-col">
        {steps.map((k, i) => (
          <li key={k} className="flex gap-3">
            <div className="flex flex-col items-center">
              <span
                className={[
                  "flex h-6 w-6 items-center justify-center rounded-[4px] font-mono text-[11px]",
                  i === 0 ? "bg-aibox-green text-aibox-bg" : "border border-aibox-line text-aibox-text2",
                ].join(" ")}
              >
                {i + 1}
              </span>
              {i < steps.length - 1 && <span className="h-8 w-px bg-aibox-line" />}
            </div>
            <div className="pb-6">
              <p className="text-sm leading-relaxed text-aibox-text1">{t(k)}</p>
              {/* 截图位占位（官网构建期静态资源，不入 registry） */}
              <div className="mt-2 flex h-24 items-center justify-center rounded-[4px] border border-dashed border-aibox-line font-mono text-[11px] text-aibox-text2">
                {t("ss.shotPlaceholder")}
              </div>
            </div>
          </li>
        ))}
      </ol>

      <p className="rounded-[6px] border border-aibox-line bg-aibox-raised p-3 font-mono text-[11px] leading-relaxed text-aibox-text2">
        {t("ss.note")}
      </p>
    </div>
  );
}
