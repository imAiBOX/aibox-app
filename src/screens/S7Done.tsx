/**
 * S7 DONE：已装内容清单 + 「下一步」教程卡 + 分享/反馈入口。
 */
import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { MachineState } from "../state";
import { appGuideUrl, siteUrl } from "../lib/links";
import { api } from "../net/client";

export default function S7Done({
  state,
  dataPaths,
  onRestart,
}: {
  state: MachineState;
  /** 该应用的数据/配置目录（registry maintenance.dataPaths；完成页告诉用户「装到哪了」） */
  dataPaths?: string[];
  onRestart: () => void;
}) {
  const { t, i18n } = useTranslation();
  /** 「打开文件夹」失败（后端只允许打开家目录内的路径）→ 可见提示，别让按钮像坏了 */
  const [revealErr, setRevealErr] = useState(false);
  /** 数据目录（可能为空：registry 未声明 maintenance.dataPaths） */
  const paths = dataPaths ?? [];
  const installed = state.tasks.filter((tk) => tk.status === "done");
  /** 跳过的组件（之前会静默消失 → 用户以为全装上了） */
  const skipped = state.tasks.filter((tk) => tk.status === "skipped");
  /** 标题按「有没有没装上的」分开说，不无脑报喜 */
  const target = installed.find((tk) => tk.role === "target");
  const unverified = target?.probeStatus !== "version" && target?.probeStatus !== "mock";
  const title = unverified
    ? t(target?.probeStatus === "present" ? "s7.presentUnverified" : "s7.installUnverified")
    : skipped.length > 0 ? t("s7.titlePartial", { count: skipped.length }) : t("s7.title");
  const subtitle = unverified
    ? t("s7.verifyManually")
    : skipped.length > 0
      ? t("s7.subtitlePartial")
      : t("s7.subtitleCount", { count: installed.length });

  /** 打开数据目录（失败 → 说人话） */
  const reveal = async (path: string) => {
    setRevealErr(false);
    try {
      await api.revealDir(path);
    } catch {
      setRevealErr(true);
    }
  };

  return (
    <div className="mx-auto flex h-full max-w-2xl flex-col gap-5">
      <div className="flex flex-col gap-1">
        <h1 className={`font-display text-2xl font-bold ${unverified ? "text-aibox-info" : "text-aibox-ok"}`}>{title}</h1>
        <p className="text-sm text-aibox-text2">{subtitle}</p>
      </div>

      {/* 跳过模型配置 → 如实告知（应用装好了，但模型还没接上） */}
      {state.configSkipped && (
        <section className="rounded-[6px] border border-aibox-info/60 bg-aibox-info/10 px-4 py-3">
          <h2 className="font-display text-sm font-semibold text-aibox-info">
            {t("s7.configSkippedTitle")}
          </h2>
          <p className="mt-1 text-sm leading-relaxed text-aibox-text2">
            {t("s7.configSkippedDesc")}
          </p>
        </section>
      )}

      {/* 装到哪了：完成页只列名字+版本 → 用户不知道自己装到哪、怎么打开。
          这里给出 registry 声明的真实数据目录，并给一个「打开」按钮。 */}
      {paths.length > 0 && (
        <section className="rounded-[6px] border border-aibox-line bg-aibox-raised px-4 py-3">
          <h2 className="mb-1 font-display text-sm font-semibold text-aibox-cream">
            {t("s7.whereTitle")}
          </h2>
          <ul className="flex flex-col gap-1.5">
            {paths.map((p) => (
              <li key={p} className="flex items-center justify-between gap-3">
                <span className="break-all font-mono text-[11px] text-aibox-text2">{p}</span>
                <button
                  type="button"
                  onClick={() => void reveal(p)}
                  className="shrink-0 rounded-[4px] border border-aibox-line px-2 py-1 font-mono text-[10px] text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
                >
                  {t("s7.openFolder")}
                </button>
              </li>
            ))}
          </ul>
          {revealErr && <p className="mt-1 text-[11px] text-aibox-danger">{t("s8.restoreRevealFail")}</p>}
          <p className="mt-2 text-[11px] leading-relaxed text-aibox-text2">{t("s7.whereHint")}</p>
        </section>
      )}

      {/* 已装清单 */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <h2 className="mb-2 font-display text-sm font-semibold text-aibox-cream">
          {t("s7.listTitle")}
        </h2>
        <ul className="flex flex-col gap-1.5">
          {installed.map((tk) => (
            <li key={tk.id} className="flex items-center justify-between font-mono text-xs">
              <span className="text-aibox-text1">
                <span>{tk.actualVersion || tk.probeStatus === "mock" ? "√ " : "? "}</span>
                {tk.name}
              </span>
              <span className="text-aibox-text2">{tk.actualVersion ?? (tk.probeStatus === "mock" ? tk.version : t("s7.versionUnavailable"))}</span>
            </li>
          ))}
          {/* 没装上的也要列出来（否则用户以为全装上了） */}
          {skipped.map((tk) => (
            <li key={tk.id} className="flex items-center justify-between font-mono text-xs">
              <span className="text-aibox-text2">
                <span>⊘ </span>
                {tk.name}
                <span className="ml-2 text-[10px] text-aibox-danger">{t("s7.skippedLabel")}</span>
              </span>
              <span className="text-aibox-text2">{tk.version}</span>
            </li>
          ))}
          {installed.length === 0 && skipped.length === 0 && (
            <li className="font-mono text-xs text-aibox-text2">{t("s7.listEmpty")}</li>
          )}
        </ul>
      </section>

      {/* 下一步教程卡 */}
      <section className="rounded-[6px] border border-aibox-green/50 bg-aibox-raised p-4">
        <h2 className="mb-1.5 font-display text-sm font-semibold text-aibox-green">
          {t("s7.nextTitle")}
        </h2>
        {!unverified && <p className="mb-3 text-sm leading-relaxed text-aibox-text2">{t("s7.nextDesc")}</p>}
        {/* 教程指向**这个应用自己的**教程页（落地页内含完整教程）；
            原先指向 /docs —— 该路径在站点上是 404（装完点一下就弹错误页）。 */}
        <a
          href={appGuideUrl(state.appId ?? "openclaw", i18n.resolvedLanguage)}
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
          href={siteUrl("/feedback/", i18n.resolvedLanguage)}
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
            void navigator.clipboard?.writeText(t("s7.shareText"));
          }}
          className="font-mono text-xs text-aibox-text2 underline hover:text-aibox-green"
        >
          {t("s7.share")}
        </button>
      </div>
    </div>
  );
}
