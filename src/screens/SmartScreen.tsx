/**
 * SmartScreen / Gatekeeper 双平台引导页：
 * Windows =「已保护你的电脑」四步；macOS = Gatekeeper「无法打开」四步（右键打开 / 隐私与安全性仍要打开）。
 * 文案 key 化（en/zh），按运行平台切换前缀（ss.* / gk.*）；截图位占位（官网构建期静态资源，不入 registry）。
 */
import { useTranslation } from "react-i18next";
import { isMacPlatform } from "../platform";

export const RELEASES_URL = "https://github.com/imAiBOX/aibox-app/releases";

export default function SmartScreen({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const mac = isMacPlatform();
  const steps = mac
    ? ["gk.step1", "gk.step2", "gk.step3", "gk.step4"]
    : ["ss.step1", "ss.step2", "ss.step3", "ss.step4"];

  return (
    <div className="mx-auto flex h-full max-w-xl flex-col gap-5 overflow-y-auto pb-6">
      <header className="flex items-center justify-between">
        <h1 className="font-display text-xl font-bold text-aibox-cream">
          {t(mac ? "gk.title" : "ss.title")}
        </h1>
        <button
          type="button"
          onClick={onClose}
          className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          {t("ss.back")}
        </button>
      </header>

      <p className="text-sm leading-relaxed text-aibox-text2">
        {t(mac ? "gk.intro" : "ss.intro")}
      </p>

      {/* 什么时候会出现警告（R1：先解释再操作，消除恐惧） */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <h2 className="mb-1.5 font-display text-sm font-semibold text-aibox-cream">
          {t(mac ? "gk.whenTitle" : "ss.whenTitle")}
        </h2>
        <p className="text-xs leading-relaxed text-aibox-text2">
          {t(mac ? "gk.whenBody" : "ss.whenBody")}
        </p>
      </section>

      {/* 竖向时间线（四步：Windows = 更多信息→仍要运行；macOS = 右键打开→仍要打开） */}
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

      {/* SHA256 核对说明位（指向 Release 页；C 档镜像安全的信任闭环） */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <h2 className="mb-1.5 font-display text-sm font-semibold text-aibox-cream">
          {t("ss.shaTitle")}
        </h2>
        <p className="mb-3 text-xs leading-relaxed text-aibox-text2">{t("ss.shaBody")}</p>
        <a
          href={RELEASES_URL}
          target="_blank"
          rel="noreferrer"
          className="inline-block rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-info hover:border-aibox-green hover:text-aibox-green"
        >
          {t("ss.shaOpen")} ↗
        </a>
      </section>

      <p className="rounded-[6px] border border-aibox-line bg-aibox-raised p-3 font-mono text-[11px] leading-relaxed text-aibox-text2">
        {t(mac ? "gk.note" : "ss.note")}
      </p>
    </div>
  );
}
