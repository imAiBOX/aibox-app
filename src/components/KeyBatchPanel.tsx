import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { appGuideUrl } from "../lib/links";
import { openExternal } from "../lib/openExternal";
import { api, type AppManifest } from "../net/client";
import { CONFIG_PROVIDERS } from "../config/config-capability-v1";
import { runConfigWrite } from "../config/flow";
import {
  buildCandidates,
  keyOk,
  summarize,
  writableSelected,
  type BatchCandidate,
  type BatchReceipt,
} from "../config/batch";
import type { ProviderId } from "../config/types";

interface Props {
  /** 已装应用（管理页列表） */
  installed: { appId: string }[];
}

/**
 * 批量「一个 key 通吃」面板。
 *
 * 设计原则（面向完全不懂技术的用户）：
 * - **先看清再动手**：列出每个应用能不能自动配，不能的明说「要手动」。
 * - **默认不勾选**：批量写配置是有后果的操作，不能默认全选。
 * - **逐个回执**：一个失败不影响其他，最后逐条告诉用户成功/失败/备份在哪。
 */
export function KeyBatchPanel({ installed }: Props) {
  const { t, i18n } = useTranslation();
  const [provider, setProvider] = useState<ProviderId>("openrouter");
  const [key, setKey] = useState("");
  const [catalog, setCatalog] = useState<{ installed: Props["installed"]; profiles: Record<string, AppManifest | undefined>; platform?: string }>();
  const candidates: BatchCandidate[] | null = useMemo(
    () => catalog?.installed === installed ? buildCandidates(installed, catalog.profiles, provider, catalog.platform) : null,
    [catalog, installed, provider],
  );
  const [selected, setSelected] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [receipts, setReceipts] = useState<BatchReceipt[] | null>(null);


  // 拉 registry 元数据（哪些应用能自动写这个 provider）
  useEffect(() => {
    let alive = true;
    if (!installed.length) return;
    void Promise.all([
      api.scanSystem().then((report) => report.os.platform, () => undefined),
      Promise.all(installed.map(async ({ appId }) => [appId, await api.registryApp(appId).catch(() => undefined)] as const)),
    ]).then(([platform, pairs]) => {
      if (alive) setCatalog({ installed, platform, profiles: Object.fromEntries(pairs) });
    });
    return () => {
      alive = false;
    };
  }, [installed]);

  const plan = useMemo(
    () => writableSelected(candidates ?? [], selected),
    [candidates, selected],
  );
  const keyValid = keyOk(key) && plan.every((candidate) =>
    keyOk(key, catalog?.profiles[candidate.appId]?.modelProviders?.[provider]?.keyFormat));
  const summary = receipts ? summarize(receipts) : null;

  const toggle = useCallback((appId: string) => {
    setSelected((prev) => (prev.includes(appId) ? prev.filter((x) => x !== appId) : [...prev, appId]));
  }, []);

  const run = useCallback(async () => {
    if (busy || plan.length === 0 || !keyValid) return;
    setBusy(true);
    setReceipts(null);
    const out: BatchReceipt[] = [];
    // 串行：逐个写、逐个备份、逐个回执（一个失败不打断其他）
    for (const c of plan) {
      const result = await runConfigWrite({ appId: c.appId, provider, apiKey: key.trim() });
      out.push({ appId: c.appId, name: c.name, ...result });
    }
    setReceipts(out);
    setBusy(false);
  }, [busy, plan, keyValid, key, provider]);

  if (installed.length === 0) return null;

  return (
    <section className="panel space-y-3">
      <div>
        <p className="font-display font-semibold text-aibox-cream">{t("batch.title")}</p>
        <p className="mt-1 text-xs leading-relaxed text-aibox-text2">{t("batch.hint")}</p>
      </div>

      {/* 1) 选服务商 */}
      <div className="flex flex-wrap gap-2">
        {CONFIG_PROVIDERS.map((id) => (
          <button
            key={id}
            type="button"
            disabled={busy || provider === id}
            onClick={() => {
              setProvider(id);
              setSelected([]);
              setReceipts(null);
              setKey("");
            }}
            className={
              "rounded-[4px] border px-3 py-1.5 font-mono text-xs " +
              (provider === id
                ? "border-aibox-green bg-aibox-green/15 text-aibox-green"
                : "border-aibox-line text-aibox-text2 hover:border-aibox-green")
            }
          >
            {id}
          </button>
        ))}
      </div>

      {/* 2) 贴 key */}
      <div>
        <input
          type="password"
          disabled={busy || !candidates?.some((candidate) => candidate.writable)}
          value={key}
          onChange={(e) => setKey(e.target.value)}
          placeholder={t("batch.keyPlaceholder")}
          aria-label={t("batch.keyPlaceholder")}
          className="w-full rounded-[4px] border border-aibox-line bg-aibox-bg px-3 py-2 font-mono text-xs text-aibox-cream outline-none focus:border-aibox-green"
        />
        {key.trim() && !keyValid && (
          <p className="mt-1 text-xs text-aibox-danger">{t("batch.keyInvalid")}</p>
        )}
      </div>

      {/* 3) 选应用（默认全不勾） */}
      {candidates === null ? (
        <p className="text-xs text-aibox-text2">{t("batch.loading")}</p>
      ) : (
        <ul className="space-y-1">
          {candidates.map((c) => (
            <li key={c.appId} className="flex items-center justify-between gap-3 text-xs">
              <label className="flex min-w-0 items-center gap-2">
                <input
                  type="checkbox"
                  disabled={!c.writable || busy}
                  checked={selected.includes(c.appId)}
                  onChange={() => toggle(c.appId)}
                />
                <span className="truncate text-aibox-text1">{c.name}</span>
              </label>
              {!c.writable && (
                <span className="shrink-0 font-mono text-[11px] text-aibox-text2">
                  {c.reason === "unknown" ? t("batch.reasonUnknown") : (
                    <button type="button" onClick={() => void openExternal(appGuideUrl(c.appId, i18n.resolvedLanguage))} className="underline">
                      {t("batch.reasonManual")}
                    </button>
                  )}
                </span>
              )}
            </li>
          ))}
        </ul>
      )}

      {/* 4) 写入 + 回执 */}
      <div className="flex items-center gap-3">
        <button
          type="button"
          disabled={busy || plan.length === 0 || !keyValid}
          onClick={() => void run()}
          className="rounded-[4px] bg-aibox-green px-4 py-2 font-display text-sm font-semibold text-aibox-bg disabled:opacity-40"
        >
          {busy ? t("batch.writing") : t("batch.write", { count: plan.length })}
        </button>
        {plan.length === 0 && !busy && (
          <span className="text-xs text-aibox-text2">{t("batch.pickHint")}</span>
        )}
      </div>

      {summary && (
        <div className="space-y-1 text-xs">
          <p className={summary.failed === 0 ? "text-aibox-green" : "text-aibox-text2"}>
            {t("batch.summary", { ok: summary.ok, failed: summary.failed })}
          </p>
          <ul className="space-y-1">
            {receipts?.map((r) => (
              <li key={r.appId} className="flex flex-wrap items-center gap-2">
                <span className={r.ok ? "text-aibox-green" : "text-aibox-danger"}>
                  {r.ok ? "✓" : "✗"} {r.name}
                </span>
                {r.ok && r.backupPath && (
                  <span className="font-mono text-[11px] text-aibox-text2">
                    {t("batch.backupAt")} {r.backupPath}
                  </span>
                )}
                {r.ok && !r.backupPath && (
                  <span className="font-mono text-[11px] text-aibox-text2">{t("s5.firstWriteNoBackup")}</span>
                )}
                {!r.ok && r.errorKey && <span className="text-aibox-text2">{t(r.errorKey)}</span>}
                {!r.ok && (
                  <button
                    type="button"
                    onClick={() =>
                      void openExternal(appGuideUrl(r.appId, i18n.resolvedLanguage))
                    }
                    className="rounded-[4px] border border-aibox-line px-2 py-0.5 font-mono text-[11px] text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
                  >
                    {t("s5.manualGuide")}
                  </button>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
