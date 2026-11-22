import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { appGuideUrl } from "../lib/links";
import { openExternal } from "../lib/openExternal";
import { api } from "../net/client";
import {
  buildCandidates,
  keyOk,
  summarize,
  writableSelected,
  type BatchCandidate,
  type BatchReceipt,
} from "../config/batch";
import { PROVIDERS } from "../config/providers";
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
  const [candidates, setCandidates] = useState<BatchCandidate[] | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [receipts, setReceipts] = useState<BatchReceipt[] | null>(null);
  const [keyFormat, setKeyFormat] = useState<string | undefined>(undefined);

  // 拉 registry 元数据（哪些应用能自动写这个 provider）
  useEffect(() => {
    let alive = true;
    setCandidates(null);
    setReceipts(null);
    setSelected([]);
    void Promise.all(
      installed.map(async (it) => {
        try {
          const m = await api.registryApp(it.appId);
          return [it.appId, m.modelProviders?.[provider]] as const;
        } catch {
          return [it.appId, undefined] as const;
        }
      }),
    ).then((pairs) => {
      if (!alive) return;
      const metas: Record<string, { configWritable?: boolean } | undefined> = {};
      for (const [id, meta] of pairs) metas[id] = meta;
      setCandidates(buildCandidates(installed, metas));
    });
    return () => {
      alive = false;
    };
  }, [installed, provider]);

  // key 格式（registry 真源）
  useEffect(() => {
    let alive = true;
    const first = candidates?.find((c) => c.writable);
    if (!first) {
      setKeyFormat(undefined);
      return;
    }
    void api
      .registryApp(first.appId)
      .then((m) => {
        if (alive) setKeyFormat(m.modelProviders?.[provider]?.keyFormat);
      })
      .catch(() => {
        if (alive) setKeyFormat(undefined);
      });
    return () => {
      alive = false;
    };
  }, [candidates, provider]);

  const plan = useMemo(
    () => writableSelected(candidates ?? [], selected),
    [candidates, selected],
  );
  const keyValid = keyOk(key, keyFormat);
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
      try {
        const r = await api.configWrite({
          appId: c.appId,
          provider,
          apiKey: key.trim(),
        });
        out.push(
          r.ok
            ? { appId: c.appId, name: c.name, ok: true, backupPath: r.backupPath }
            : {
                appId: c.appId,
                name: c.name,
                ok: false,
                errorKey: "err.configFailed",
                errorCode: "CONFIG_FAILED",
              },
        );
      } catch (e) {
        const code = extractCode(e);
        out.push({
          appId: c.appId,
          name: c.name,
          ok: false,
          errorKey: "err.configFailed",
          errorCode: code,
        });
      }
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
        {PROVIDERS.map((p) => (
          <button
            key={p.id}
            type="button"
            onClick={() => setProvider(p.id as ProviderId)}
            className={
              "rounded-[4px] border px-3 py-1.5 font-mono text-xs " +
              (provider === p.id
                ? "border-aibox-green bg-aibox-green/15 text-aibox-green"
                : "border-aibox-line text-aibox-text2 hover:border-aibox-green")
            }
          >
            {p.id}
          </button>
        ))}
      </div>

      {/* 2) 贴 key */}
      <div>
        <input
          type="password"
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
                  {c.reason === "unknown" ? t("batch.reasonUnknown") : t("batch.reasonManual")}
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

/** 从 Tauri 报错里取机器码（形如 `CONFIG_XXX: 人话`）。 */
function extractCode(e: unknown): string | undefined {
  const s = typeof e === "string" ? e : ((e as { message?: string })?.message ?? "");
  const m = s.trim().match(/^([A-Z][A-Z0-9_]{3,})\b/);
  return m?.[1];
}
