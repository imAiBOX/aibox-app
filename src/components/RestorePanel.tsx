/** Show the local backup library, including backups for apps no longer installed. */
import { useCallback, useEffect, useMemo, useReducer, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, type BackupRecord } from "../net/client";
import { createInitialRestoreState, formatBytes, restoreReduce } from "../state/restore";

/** 备份创建时间 → 人话时间（清单没写时间 → 破折号，不猜） */
function whenText(b: BackupRecord, lang: string): string {
  const ms = b.createdAtMs || (b.createdAt ? Date.parse(b.createdAt) : NaN);
  if (!Number.isFinite(ms) || ms <= 0) return "—";
  try {
    return new Date(ms).toLocaleString(lang);
  } catch {
    return new Date(ms).toISOString();
  }
}

export function RestorePanel() {
  const { t, i18n } = useTranslation();
  const [state, dispatch] = useReducer(restoreReduce, undefined, createInitialRestoreState);
  /** 应用名（registry；拿不到就用 appId 兜底） */
  const [names, setNames] = useState<ReadonlyMap<string, string>>(new Map());
  /** 「打开备份文件夹」失败（后端只允许打开家目录内的路径）→ 给一句人话，别静默 */
  const [revealErr, setRevealErr] = useState(false);

  /** 打开备份文件夹（失败 → 可见提示，而不是点了没反应） */
  const reveal = useCallback(async (dir: string) => {
    setRevealErr(false);
    try {
      await api.revealDir(dir);
    } catch {
      setRevealErr(true);
    }
  }, []);

  const load = useCallback(async () => {
    dispatch({ type: "R_LIST_LOADING" });
    try {
      const list = await api.manageBackupList();
      dispatch({ type: "R_LIST_LOADED", backups: list });
    } catch {
      dispatch({ type: "R_LIST_ERROR" });
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const rows = useMemo(
    () => [...state.backups].sort((a, b) => (b.createdAtMs || 0) - (a.createdAtMs || 0)),
    [state.backups],
  );

  /** 应用名（一次批量取；失败不影响主流程） */
  const appIds = useMemo(() => Array.from(new Set(rows.map((b) => b.appId))), [rows]);
  useEffect(() => {
    let alive = true;
    if (appIds.length === 0) return;
    void Promise.all(
      appIds.map(async (id) => {
        try {
          const m = await api.registryApp(id);
          return [id, m.name] as const;
        } catch {
          return [id, ""] as const;
        }
      }),
    ).then((pairs) => {
      if (!alive) return;
      setNames(new Map(pairs.filter(([, name]) => name)));
    });
    return () => {
      alive = false;
    };
  }, [appIds]);

  return (
    <section className="panel space-y-3">
      <div>
        <p className="font-display font-semibold text-aibox-cream">{t("s8.restorePanelTitle")}</p>
        <p className="mt-1 text-xs leading-relaxed text-aibox-text2">{t("s8.errRestorePaused")}</p>
      </div>

      {state.step === "listError" && (
        <div className="flex items-center justify-between gap-3 text-sm text-aibox-danger">
          <span>{t("s8.restoreLoadErr")}</span>
          <button
            type="button"
            onClick={() => void load()}
            className="shrink-0 rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
          >
            {t("s8.restoreReload")}
          </button>
        </div>
      )}

      {state.step !== "listError" && rows.length === 0 && state.step !== "listing" && (
        <p className="text-xs text-aibox-text2">{t("s8.restoreNoBackups")}</p>
      )}

      {revealErr && (
        <p className="text-xs text-aibox-danger">{t("s8.restoreRevealFail")}</p>
      )}

      {rows.length > 0 && (
        <ul className="space-y-2">
          {rows.map((b) => {
            const hint = b.format === "legacy" ? t("s8.restoreLegacy")
              : b.format === "unknown" ? t("s8.restoreNoManifest")
              : "";
            return (
              <li
                key={b.dir}
                className="flex flex-wrap items-center justify-between gap-3 rounded-[6px] border border-aibox-line bg-aibox-raised px-4 py-3"
              >
                <div className="min-w-0">
                  <p className="font-display font-semibold text-aibox-cream">{names.get(b.appId) ?? b.appId}</p>
                  <p className="font-mono text-[11px] text-aibox-text2">
                    {t("s8.restoreEntry", {
                      when: whenText(b, i18n.resolvedLanguage ?? "en"),
                      count: b.fileCount,
                      size: formatBytes(b.totalBytes),
                    })}
                  </p>
                  {hint && <p className="mt-1 text-xs text-aibox-danger">{hint}</p>}
                </div>
                <div className="flex shrink-0 flex-wrap gap-2">
                  <button
                    type="button"
                    onClick={() => void reveal(b.dir)}
                    className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-[10px] text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
                  >
                    {t("s8.restoreOpenFolder")}
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
