/**
 * 还原面板（MANAGE 列表里的一块）。
 *
 * **为什么单独一个面板**：用户想「把数据放回去」时，入口必须一眼可见——
 * 埋在某个应用的「卸载」菜单里没人找得到。
 *
 * 设计原则（面向完全不懂技术的用户）：
 * - **只列已装应用的备份**：给没装的应用显示备份是噪音（也容易让人误装）。
 * - **能还原 / 不能还原，写清楚**：不可还原的备份给出**可见小字**说明原因
 *   （禁用按钮不会派发鼠标事件 → title 悬浮提示在小浏览器里根本不出现）。
 * - **旧格式备份不给按钮**：旧备份没有还原清单，硬还原会出事 → 只提示手动恢复。
 */
import { useCallback, useEffect, useMemo, useReducer, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, type BackupRecord } from "../net/client";
import { createInitialRestoreState, formatBytes, restoreReduce } from "../state/restore";
interface Props {
  /** 已装应用（管理页列表） */
  installed: { appId: string }[];
  /** 点「还原」→ 打开全屏还原流程 */
  onRestore: (backupDir: string, appId: string) => void;
}

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

export function RestorePanel({ installed, onRestore }: Props) {
  const { t, i18n } = useTranslation();
  const [state, dispatch] = useReducer(restoreReduce, undefined, createInitialRestoreState);
  /** 应用名（registry；拿不到就用 appId 兜底） */
  const [names, setNames] = useState<Record<string, string>>({});
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

  /** 只显示已装应用的备份 */
  const installedIds = useMemo(() => new Set(installed.map((i) => i.appId)), [installed]);
  const rows = useMemo(
    () =>
      state.backups
        .filter((b) => installedIds.has(b.appId))
        .sort((a, b) => (b.createdAtMs || 0) - (a.createdAtMs || 0)),
    [state.backups, installedIds],
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
      const map: Record<string, string> = {};
      for (const [id, name] of pairs) if (name) map[id] = name;
      setNames(map);
    });
    return () => {
      alive = false;
    };
  }, [appIds]);

  // 没有任何已装应用 → 整个面板不出现（没有可还原的对象）
  if (installed.length === 0) return null;

  return (
    <section className="panel space-y-3">
      <div>
        <p className="font-display font-semibold text-aibox-cream">{t("s8.restorePanelTitle")}</p>
        <p className="mt-1 text-xs leading-relaxed text-aibox-text2">{t("s8.restorePanelLede")}</p>
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
            // 三种「不能还原」要分开说：
            // legacy = 旧版本格式（没有还原清单）；unknown = 备份不完整（根本没有清单）；
            // 其余（太旧 / 来自别的电脑 / 时间异常）→ 逐条给出人话原因。
            const legacy = b.format === "legacy";
            const noManifest = b.format === "unknown";
            const hint = legacy
              ? t("s8.restoreLegacy")
              : noManifest
                ? t("s8.restoreNoManifest")
                : !b.restorable
                  ? b.ageHours > 24
                    ? t("s8.restoreTooOld")
                    : b.ageHours === 0
                      ? t("s8.restoreBadTime")
                      : t("s8.restoreUnsupported")
                  : "";
            return (
              <li
                key={b.dir}
                className="flex flex-wrap items-center justify-between gap-3 rounded-[6px] border border-aibox-line bg-aibox-raised px-4 py-3"
              >
                <div className="min-w-0">
                  <p className="font-display font-semibold text-aibox-cream">{names[b.appId] ?? b.appId}</p>
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
                  {/* 旧格式 / 不完整的备份不给还原按钮（没有还原清单，硬还原会出事） */}
                  {b.format === "v1" && (
                    <button
                      type="button"
                      disabled={!b.restorable}
                      onClick={() => onRestore(b.dir, b.appId)}
                      className="rounded-[4px] border border-aibox-danger/70 px-3 py-1.5 font-mono text-xs text-aibox-danger hover:border-aibox-danger disabled:opacity-40"
                    >
                      {t("s8.restoreConfirmBtn")}
                    </button>
                  )}
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
