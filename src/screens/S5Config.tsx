/**
 * S5 CONFIG 模型配置向导：
 *  C1 选择六卡片等权（推荐=右上角小标签 + 返佣明示，不整卡变色）
 *  C2「去哪弄 key」陪跑页（官网入口 + 注册→充值→创建 key 分步占位 + key 安全三条）
 *  C3 粘贴 key（等宽输入 + 即时格式校验，registry keyFormat 真源；人话纠错）
 *  C4 选择模型（registry recommendedModels 短清单 + 一句话 + priceHint）
 *  C5 写入（config_write：备份→合并→回读；展示 backupPath 与「已备份原文件」→ VERIFY）
 *  Google / Claude-OAuth / Ollama = TODO 占位（点入「Phase 后续」人话说明，不装能用）
 *
 * 流程逻辑在 src/config/flow.ts（纯 reducer，可 vitest 驱动），本文件只做渲染与事件。
 * key 只活在 flow 内存态：不进主 reducer、不进持久化、不进日志（硬规则）。
 */
import { useCallback, useEffect, useReducer } from "react";
import { openExternal } from "../lib/openExternal";
import { useTranslation } from "react-i18next";
import { PROVIDERS, maskKey, priceHintKey, providerById } from "../config/providers";
import {
  createWizard,
  reduceWizard,
  runConfigWrite,
  wizardKeyCheck,
  wizardMeta,
  wizardModels,
  type WizardState,
} from "../config/flow";
import type { ProviderId } from "../config/types";
import { api } from "../net/client";

export interface ConfigCreds {
  provider: ProviderId;
  apiKey?: string;
  model?: string;
}

/** 步骤导航条（5 步人话标签） */
function StepNav({ step }: { step: WizardState["step"] }) {
  const { t } = useTranslation();
  const order: WizardState["step"][] = ["C1", "C2", "C3", "C4", "C5"];
  const cur = order.indexOf(step);
  return (
    <div className="flex flex-wrap gap-1.5 font-mono text-[11px]">
      {order.map((s, i) => (
        <span
          key={s}
          className={[
            "rounded-[3px] border px-1.5 py-0.5",
            i === cur
              ? "border-aibox-green text-aibox-green"
              : i < cur
                ? "border-aibox-line text-aibox-ok"
                : "border-aibox-line text-aibox-text2",
          ].join(" ")}
        >
          {t(`s5.nav${i + 1}`)}
        </span>
      ))}
    </div>
  );
}

function BackBtn({ onClick }: { onClick: () => void }) {
  const { t } = useTranslation();
  return (
    <button
      type="button"
      onClick={onClick}
      className="rounded-[4px] border border-aibox-line px-4 py-2 font-mono text-xs text-aibox-text2 hover:border-aibox-text2"
    >
      {t("btn.back")}
    </button>
  );
}

export default function S5Config({
  appId,
  selectedPathId,
  onSelectPath,
  onWritten,
}: {
  appId: string | null;
  selectedPathId: string | null;
  onSelectPath: (id: string) => void;
  onWritten: (id: string, creds: ConfigCreds) => void;
}) {
  const { t } = useTranslation();
  const [w, dispatch] = useReducer(reduceWizard, undefined, createWizard);
  const provider = providerById(w.providerId ?? selectedPathId);

  // registry 真数据：C3 keyFormat + C4 recommendedModels（拉不到 → 本地回落，不阻塞）
  useEffect(() => {
    if (!appId) return;
    let alive = true;
    api
      .registryApp(appId)
      .then((m) => {
        if (alive && m.modelProviders) {
          dispatch({ type: "SET_META", metas: m.modelProviders });
        }
      })
      .catch(() => undefined); // mock/网络缺席 → 本地 keyPattern + fallbackModels
    return () => {
      alive = false;
    };
  }, [appId]);

  const pick = useCallback(
    (id: string) => {
      dispatch({ type: "PICK", providerId: id });
      onSelectPath(id);
    },
    [onSelectPath],
  );

  const keyCheck = wizardKeyCheck(w);
  const models = wizardModels(w);

  // C5 写入（真 invoke / mock 由 client 单一入口决定）
  const doWrite = useCallback(async () => {
    if (!w.providerId || w.writing) return;
    dispatch({ type: "WRITE_START" });
    const out = await runConfigWrite({
      appId: appId ?? "openclaw",
      provider: w.providerId as ProviderId,
      apiKey: w.key || undefined,
      model: w.model ?? undefined,
    });
    if (out.ok) {
      dispatch({ type: "WRITE_DONE", ok: true, backupPath: out.backupPath });
    } else {
      dispatch({ type: "WRITE_FAIL", errorKey: out.errorKey ?? "s5.writeFail" });
    }
  }, [appId, w.providerId, w.writing, w.key, w.model]);

  const finish = useCallback(() => {
    if (!w.providerId) return;
    onWritten(w.providerId, {
      provider: w.providerId as ProviderId,
      apiKey: w.key || undefined,
      model: w.model ?? undefined,
    });
  }, [onWritten, w.providerId, w.key, w.model]);

  return (
    <div className="flex h-full flex-col gap-4">
      <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s5.title")}</h1>
      <p className="text-sm text-aibox-text2">{t("s5.subtitle")}</p>
      {w.step !== "C1" && w.step !== "TODO" && <StepNav step={w.step} />}

      {/* ── C1 选择路径：六卡片等权重 ── */}
      {w.step === "C1" && (
        <div className="grid gap-3 [grid-template-columns:repeat(auto-fill,minmax(240px,1fr))]">
          {PROVIDERS.map((p) => (
            <button
              key={p.id}
              type="button"
              onClick={() => pick(p.id)}
              className={[
                "relative flex flex-col gap-1.5 rounded-[6px] border bg-aibox-raised p-4 text-left transition-colors",
                (w.providerId ?? selectedPathId) === p.id
                  ? "border-aibox-green"
                  : "border-aibox-line hover:border-aibox-text2",
              ].join(" ")}
            >
              {/* 推荐徽章 = 右上角小标签（不是整卡变色，避免诱导感，S5） */}
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
      )}

      {/* ── TODO 占位（google / claude-oauth / ollama）：点入人话说明，不装能用 ── */}
      {w.step === "TODO" && provider && (
        <section className="flex flex-col gap-3 rounded-[6px] border border-dashed border-aibox-line bg-aibox-raised p-5">
          <h2 className="font-display text-base font-semibold text-aibox-cream">
            {t(provider.nameKey)} · {t("s5.todoTitle")}
          </h2>
          <p className="text-sm leading-relaxed text-aibox-text2">{t("s5.todoDetail")}</p>
          <p className="font-mono text-xs text-aibox-text2">{t(provider.descKey)}</p>
          <div className="mt-1">
            <BackBtn onClick={() => dispatch({ type: "BACK" })} />
          </div>
        </section>
      )}

      {/* ── C2「去哪弄 key」陪跑页 ── */}
      {w.step === "C2" && provider && (
        <section className="flex flex-col gap-4">
          <h2 className="font-display text-base font-semibold text-aibox-cream">
            {t("s5.guideTitle")} · {t(provider.nameKey)}
          </h2>

          {/* 官网入口按钮（URL：registry getKeyUrl 优先，本地 siteUrl 兜底） */}
          <div>
            <button
              type="button"
              onClick={() =>
                void openExternal(wizardMeta(w)?.getKeyUrl ?? provider.siteUrl ?? "https://aibox.im/")
              }
              className="rounded-[4px] bg-aibox-green px-5 py-2 font-display text-sm font-semibold text-aibox-bg"
            >
              {t("s5.openSite")}
            </button>
            <span className="ml-3 font-mono text-[11px] text-aibox-text2">
              {wizardMeta(w)?.getKeyUrl ?? provider.siteUrl}
            </span>
          </div>

          {/* 注册→充值→创建 key 分步步骤（占位；截图素材 = 官网构建期静态资源，不入 registry，
              路径约定 /guides/<provider>/step1.png …，就位后替换下面的占位框） */}
          <ol className="flex flex-col gap-2">
            {["s5.stepReg", "s5.stepTopup", "s5.stepCreateKey"].map((k, i) => (
              <li
                key={k}
                className="flex items-start gap-3 rounded-[6px] border border-aibox-line bg-aibox-raised p-3"
              >
                <span className="mt-0.5 font-mono text-xs text-aibox-green">{i + 1}</span>
                <div className="min-w-0 flex-1">
                  <p className="text-sm text-aibox-text1">{t(k)}</p>
                  {/* 截图占位（官网构建期静态资源） */}
                  <div className="mt-1.5 rounded-[4px] border border-dashed border-aibox-line px-3 py-2 font-mono text-[11px] text-aibox-text2">
                    {t("s5.shotNote")}
                  </div>
                </div>
              </li>
            ))}
          </ol>

          {/* key 安全三条 */}
          <div className="rounded-[6px] border border-aibox-info/40 bg-aibox-info/5 p-3">
            <p className="mb-1.5 font-display text-sm font-semibold text-aibox-info">
              {t("s5.safetyTitle")}
            </p>
            <ul className="flex flex-col gap-1 text-sm text-aibox-text1">
              <li>1 · {t("s5.safety1")}</li>
              <li>2 · {t("s5.safety2")}</li>
              <li>3 · {t("s5.safety3")}</li>
            </ul>
          </div>

          <div className="flex justify-between">
            <BackBtn onClick={() => dispatch({ type: "BACK" })} />
            <button
              type="button"
              onClick={() => dispatch({ type: "NEXT" })}
              className="rounded-[4px] bg-aibox-green px-5 py-2 font-display text-sm font-semibold text-aibox-bg"
            >
              {t("s5.pasteTitle")}
            </button>
          </div>
        </section>
      )}

      {/* ── C3 粘贴 key：等宽 + 即时格式反馈 ── */}
      {w.step === "C3" && provider && (
        <section className="flex flex-col gap-3">
          <h2 className="font-display text-base font-semibold text-aibox-cream">
            {t("s5.pasteTitle")} · {t(provider.nameKey)}
          </h2>
          {/* 术语首次出现括注人话 */}
          <p className="text-sm text-aibox-text2">{t("s5.pasteHint")}</p>
          <input
            id="aibox-key"
            type="password"
            autoComplete="off"
            spellCheck={false}
            value={w.key}
            placeholder={provider.keyPlaceholder}
            onChange={(e) => dispatch({ type: "SET_KEY", key: e.target.value })}
            className="w-full rounded-[4px] border border-aibox-line bg-aibox-bg px-3 py-2 font-mono text-sm text-aibox-text1 outline-none focus:border-aibox-green"
          />
          {/* 即时反馈：正确=绿点+人话，错误=具体建议 */}
          {w.key.length > 0 && keyCheck && (
            <p
              className={[
                "font-mono text-xs",
                keyCheck.ok ? "text-aibox-ok" : "text-aibox-danger",
              ].join(" ")}
            >
              {keyCheck.ok ? "● " : "! "}
              {keyCheck.ok ? t("s5.keyOk") : t(keyCheck.msgKey!, keyCheck.params)}
            </p>
          )}
          <div className="mt-1 flex justify-between">
            <BackBtn onClick={() => dispatch({ type: "BACK" })} />
            <button
              type="button"
              disabled={!keyCheck?.ok}
              onClick={() => dispatch({ type: "NEXT" })}
              className="rounded-[4px] bg-aibox-green px-5 py-2 font-display text-sm font-semibold text-aibox-bg disabled:cursor-not-allowed disabled:opacity-40"
            >
              {t("s5.c4Title")}
            </button>
          </div>
        </section>
      )}

      {/* ── C4 选择模型：registry 推荐短清单 ── */}
      {w.step === "C4" && provider && (
        <section className="flex flex-col gap-3">
          <h2 className="font-display text-base font-semibold text-aibox-cream">
            {t("s5.c4Title")}
          </h2>
          {provider.modelHintKey && (
            <p className="text-sm text-aibox-text2">{t(provider.modelHintKey)}</p>
          )}
          <div className="flex flex-col gap-2">
            {models.map((m) => (
              <button
                key={m.id}
                type="button"
                onClick={() => dispatch({ type: "SET_MODEL", model: m.id })}
                className={[
                  "flex items-center justify-between gap-3 rounded-[6px] border bg-aibox-raised px-4 py-3 text-left",
                  w.model === m.id ? "border-aibox-green" : "border-aibox-line hover:border-aibox-text2",
                ].join(" ")}
              >
                <span className="min-w-0">
                  <span className="block font-display text-sm font-semibold text-aibox-text1">
                    {m.name}
                  </span>
                  {m.note && (
                    <span className="block text-xs text-aibox-text2">{m.note}</span>
                  )}
                  <span className="block font-mono text-[11px] text-aibox-text2/70">{m.id}</span>
                </span>
                {m.priceHint && (
                  <span className="shrink-0 rounded-[3px] border border-aibox-line px-1.5 py-0.5 font-mono text-[10px] text-aibox-info">
                    {priceHintKey(m.priceHint) ? t(priceHintKey(m.priceHint)!) : m.priceHint}
                  </span>
                )}
              </button>
            ))}
            {models.length === 0 && (
              <p className="font-mono text-xs text-aibox-text2">{t("s5.modelsEmpty")}</p>
            )}
          </div>
          <div className="mt-1 flex justify-between">
            <BackBtn onClick={() => dispatch({ type: "BACK" })} />
            <button
              type="button"
              disabled={!w.model}
              onClick={() => dispatch({ type: "NEXT" })}
              className="rounded-[4px] bg-aibox-green px-5 py-2 font-display text-sm font-semibold text-aibox-bg disabled:cursor-not-allowed disabled:opacity-40"
            >
              {t("s5.write")}
            </button>
          </div>
        </section>
      )}

      {/* ── C5 写入：备份→合并→回读；成功展示 backupPath ── */}
      {w.step === "C5" && provider && (
        <section className="flex flex-col gap-3">
          <h2 className="font-display text-base font-semibold text-aibox-cream">
            {t("s5.writeTitle")}
          </h2>
          {!w.writeResult && (
            <>
              <div className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4 text-sm text-aibox-text2">
                <p>
                  <span className="font-mono text-xs">{t("s5.keyLabel")}</span>{" "}
                  <span className="font-mono text-aibox-text1">{maskKey(w.key)}</span>
                </p>
                <p className="mt-1">
                  <span className="font-mono text-xs">{t("s5.c4Title")}</span>{" "}
                  <span className="font-mono text-aibox-text1">{w.model ?? "—"}</span>
                </p>
                <p className="mt-2 text-xs">{t("s5.writeNote")}</p>
              </div>
              {w.errorKey && (
                <p className="rounded-[6px] border border-aibox-danger px-3 py-2 text-sm text-aibox-danger">
                  {t(w.errorKey)}
                </p>
              )}
              <div className="mt-1 flex justify-between">
                <BackBtn onClick={() => dispatch({ type: "BACK" })} />
                <button
                  type="button"
                  disabled={w.writing}
                  onClick={() => void doWrite()}
                  className="rounded-[4px] bg-aibox-green px-5 py-2 font-display text-sm font-semibold text-aibox-bg disabled:cursor-not-allowed disabled:opacity-40"
                >
                  {w.writing ? t("s5.writing") : t("s5.write")}
                </button>
              </div>
            </>
          )}
          {w.writeResult && (
            <div className="flex flex-col gap-3 rounded-[6px] border border-aibox-ok/60 bg-aibox-ok/10 p-4">
              <p className="font-display text-sm font-semibold text-aibox-ok">
                ✓ {t("s5.backupOk")}
              </p>
              <p className="font-mono text-xs text-aibox-text1 break-all">
                {t("s5.backupLabel")}: {w.writeResult.backupPath}
              </p>
              <p className="text-sm text-aibox-text1">{t("s5.written")}</p>
              <div>
                <button
                  type="button"
                  onClick={finish}
                  className="rounded-[4px] bg-aibox-green px-5 py-2 font-display text-sm font-semibold text-aibox-bg"
                >
                  {t("s5.toVerify")}
                </button>
              </div>
            </div>
          )}
        </section>
      )}
    </div>
  );
}
