/**
 * S5 CONFIG 模型配置向导：C1 六卡片等权（推荐=右上角小标签+返佣明示）。
 * 3 完整（openrouter/anthropic/openai）+ 3 TODO 占位（google/claude-oauth/ollama）。
 * C3 key 输入等宽 + 即时格式反馈。
 */
import { useState } from "react";
import { useTranslation } from "react-i18next";
import { PROVIDERS, providerById, validateKey } from "../config/providers";

export default function S5Config({
  selectedPathId,
  onSelectPath,
  onWritten,
}: {
  selectedPathId: string | null;
  onSelectPath: (id: string) => void;
  onWritten: (id: string) => void;
}) {
  const { t } = useTranslation();
  const [key, setKey] = useState("");
  const [touched, setTouched] = useState(false);
  const provider = providerById(selectedPathId);
  const check = provider ? validateKey(provider, key) : { ok: false, msgKey: undefined };
  const feedback = touched && provider && !provider.noKey ? check : null;

  return (
    <div className="flex h-full flex-col gap-4">
      <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s5.title")}</h1>
      <p className="text-sm text-aibox-text2">{t("s5.subtitle")}</p>

      {/* C1 六卡片等权重网格 */}
      <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(240px,1fr))]">
        {PROVIDERS.map((p) => (
          <button
            key={p.id}
            type="button"
            onClick={() => onSelectPath(p.id)}
            className={[
              "relative flex flex-col gap-1.5 rounded-[6px] border bg-aibox-raised p-4 text-left transition-colors",
              selectedPathId === p.id ? "border-aibox-green" : "border-aibox-line hover:border-aibox-text2",
            ].join(" ")}
          >
            {/* 推荐徽章 = 右上角小标签（不是整卡变色） */}
            {p.recommended && (
              <span className="absolute right-2 top-2 rounded-[3px] border border-aibox-green px-1.5 py-0.5 font-mono text-[10px] text-aibox-green">
                {t("s5.recommended")}
              </span>
            )}
            <span className="font-display text-sm font-semibold text-aibox-text1">
              {t(p.nameKey)}
            </span>
            <span className="pr-16 text-xs leading-relaxed text-aibox-text2">{t(p.descKey)}</span>
            {p.recommended && (
              <span className="font-mono text-[10px] text-aibox-info">{t("s5.rebate")}</span>
            )}
            {p.status === "todo" && (
              <span className="mt-1 self-start rounded-[3px] border border-aibox-line px-1.5 py-0.5 font-mono text-[10px] text-aibox-text2">
                {t("s5.todo")}
              </span>
            )}
          </button>
        ))}
      </div>

      {/* C3 key 输入（完整路径才显示） */}
      {provider && provider.status === "full" && (
        <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
          <label
            htmlFor="aibox-key"
            className="mb-1.5 block font-mono text-xs text-aibox-text2"
          >
            {t("s5.keyLabel")}
          </label>
          <input
            id="aibox-key"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={key}
            placeholder={provider.keyPlaceholder}
            onChange={(e) => {
              setKey(e.target.value);
              setTouched(true);
            }}
            className="w-full rounded-[4px] border border-aibox-line bg-aibox-bg px-3 py-2 font-mono text-sm text-aibox-text1 outline-none focus:border-aibox-green"
          />
          {feedback && (
            <p
              className={[
                "mt-1.5 font-mono text-xs",
                feedback.ok ? "text-aibox-ok" : "text-aibox-danger",
              ].join(" ")}
            >
              {feedback.ok ? "● " : "! "}
              {feedback.msgKey ? t(feedback.msgKey) : t("s5.keyOk")}
            </p>
          )}
          <button
            type="button"
            disabled={!check.ok}
            onClick={() => onWritten(provider.id)}
            className="mt-3 rounded-[4px] bg-aibox-green px-5 py-2 font-display text-sm font-semibold text-aibox-bg disabled:cursor-not-allowed disabled:opacity-40"
          >
            {t("s5.write")}
          </button>
        </section>
      )}

      {provider && provider.status === "todo" && (
        <p className="rounded-[6px] border border-dashed border-aibox-line p-4 font-mono text-xs text-aibox-text2">
          {t("s5.todoNote")}
        </p>
      )}
    </div>
  );
}
