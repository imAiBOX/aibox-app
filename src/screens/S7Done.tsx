/**
 * S7 DONE：已装内容清单 + 「下一步」教程卡 + 分享/反馈入口。
 */
import { useTranslation } from "react-i18next";
import type { MachineState } from "../state";

export default function S7Done({
  state,
  onRestart,
}: {
  state: MachineState;
  onRestart: () => void;
}) {
  const { t } = useTranslation();
  const installed = state.tasks.filter((tk) => tk.status === "done");

  return (
    <div className="mx-auto flex h-full max-w-2xl flex-col gap-5">
      <div className="flex flex-col gap-1">
        <h1 className="font-display text-2xl font-bold text-aibox-ok">{t("s7.title")}</h1>
        <p className="text-sm text-aibox-text2">{t("s7.subtitle")}</p>
      </div>

      {/* 已装清单 */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <h2 className="mb-2 font-display text-sm font-semibold text-aibox-cream">
          {t("s7.listTitle")}
        </h2>
        <ul className="flex flex-col gap-1.5">
          {installed.map((tk) => (
            <li key={tk.id} className="flex items-center justify-between font-mono text-xs">
              <span className="text-aibox-text1">
                <span className="text-aibox-ok">√ </span>
                {tk.name}
              </span>
              <span className="text-aibox-text2">{tk.version}</span>
            </li>
          ))}
          {installed.length === 0 && (
            <li className="font-mono text-xs text-aibox-text2">{t("s7.listEmpty")}</li>
          )}
        </ul>
      </section>

      {/* 下一步教程卡 */}
      <section className="rounded-[6px] border border-aibox-green/50 bg-aibox-raised p-4">
        <h2 className="mb-1.5 font-display text-sm font-semibold text-aibox-green">
          {t("s7.nextTitle")}
        </h2>
        <p className="mb-3 text-sm leading-relaxed text-aibox-text2">{t("s7.nextDesc")}</p>
        <a
          href="https://aibox.im/docs"
          target="_blank"
          rel="noreferrer"
          className="inline-block rounded-[4px] bg-aibox-green px-4 py-2 font-display text-sm font-semibold text-aibox-bg"
        >
          {t("s7.nextCta")}
        </a>
      </section>

      {/* 分享 / 反馈 */}
      <div className="flex items-center gap-3">
        <button
          type="button"
          onClick={onRestart}
          className="rounded-[4px] border border-aibox-line px-4 py-2 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          {t("s7.again")}
        </button>
        <a
          href="https://aibox.im/feedback"
          target="_blank"
          rel="noreferrer"
          className="font-mono text-xs text-aibox-text2 underline hover:text-aibox-green"
        >
          {t("s7.feedback")}
        </a>
        <span className="font-mono text-xs text-aibox-text2">·</span>
        <button
          type="button"
          onClick={() => {
            const text = "I installed an AI with AiBOX.im";
            void navigator.clipboard?.writeText(text);
          }}
          className="font-mono text-xs text-aibox-text2 underline hover:text-aibox-green"
        >
          {t("s7.share")}
        </button>
      </div>
    </div>
  );
}
