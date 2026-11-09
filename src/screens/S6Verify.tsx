/**
 * S6 VERIFY：仿聊天窗两条气泡（用户 "Say OK" / 模型回复 + 延迟）。
 * 成功判定 = 真实收到模型文本（不接受「无报错」弱判定）。
 */
import { useTranslation } from "react-i18next";

export default function S6Verify({
  result,
  pending,
  latencyMs,
  error,
  onRetry,
}: {
  result: string | null;
  pending: boolean;
  latencyMs: number | null;
  error: string | null;
  onRetry: () => void;
}) {
  const { t } = useTranslation();

  return (
    <div className="mx-auto flex h-full max-w-lg flex-col gap-4">
      <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s6.title")}</h1>
      <p className="text-sm text-aibox-text2">{t("s6.subtitle")}</p>

      <div className="flex flex-col gap-3 rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        {/* 用户气泡 */}
        <div className="self-end rounded-[6px] rounded-br-none bg-aibox-info/15 px-3 py-2 font-mono text-sm text-aibox-text1">
          Say OK
        </div>

        {/* 模型气泡 */}
        {pending && (
          <div className="self-start rounded-[6px] rounded-bl-none border border-aibox-line px-3 py-2 font-mono text-sm text-aibox-text2 animate-pulse">
            {t("s6.pending")}
          </div>
        )}
        {result && (
          <div className="self-start rounded-[6px] rounded-bl-none border border-aibox-ok/60 bg-aibox-ok/10 px-3 py-2 font-mono text-sm text-aibox-text1">
            {result}
            {latencyMs !== null && (
              <span className="ml-2 font-mono text-[11px] text-aibox-text2">{latencyMs} ms</span>
            )}
          </div>
        )}
        {error && (
          <div className="self-start rounded-[6px] rounded-bl-none border border-aibox-danger px-3 py-2 font-mono text-sm text-aibox-danger">
            {error}
            <button
              type="button"
              onClick={onRetry}
              className="ml-3 underline hover:text-aibox-cream"
            >
              {t("s6.retry")}
            </button>
          </div>
        )}
      </div>

      {result && (
        <p className="font-display text-lg font-semibold text-aibox-ok">{t("s6.success")}</p>
      )}
    </div>
  );
}
