/**
 * S8 MANAGE 维护模式（产品规格 §5 / ADR-07）：
 * 已装应用 → 检查更新 | 升级 | 卸载 | 重装；破坏性操作前置风险卡（人话列数据路径 + 默认备份）。
 * - 升级 / 重装后的「重新安装」= 跳回标准安装流程重跑（rerun-dag 语义，复用 EXECUTE 四阶段与视觉）
 * - 卸载 / 重装的删除部分 = manage_remove（backupFirst 硬规则在 Rust 侧强制；UI 负责风险卡守卫）
 * - 路径清单只来自 registry maintenance（buildRemovePaths 硬规则④）
 */
import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, type AppManifest, type AppUpdateRow, type ManifestEntry } from "../net/client";
import {
  buildRemovePaths,
  canConfirmRisk,
  createInitialManageState,
  isRetryableManageError,
  manageErrorKey,
  manageReduce,
  type MaintenanceInfo,
} from "../state/manage";
import { errorCodeOf } from "../state/orchestrate";

interface Props {
  onClose: () => void;
  /** 升级/重装后的「去安装」：跳标准安装流程（rerun） */
  onGotoInstall: (appId: string) => void;
}

interface Row {
  entry: ManifestEntry;
  maintenance?: MaintenanceInfo;
  update?: AppUpdateRow;
}

/** 本次破坏性操作的参数（供「重试」原样重发，不重新组路径） */
interface RemoveAttempt {
  appId: string;
  residualPaths: string[];
  dataPaths: string[];
  steps: string[];
  backup: boolean;
}

export default function S8Manage({ onClose, onGotoInstall }: Props) {
  const { t } = useTranslation();
  const [m, dispatch] = useReducer(manageReduce, undefined, createInitialManageState);
  const [rows, setRows] = useState<Row[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [loadErr, setLoadErr] = useState(false);
  /** 本次删除尝试的参数（重试时原样重发） */
  const attemptRef = useRef<RemoveAttempt | null>(null);

  const reload = useCallback(async () => {
    try {
      const [manifest, updates] = await Promise.all([api.manageManifestList(), api.checkAppUpdates().catch(() => [] as AppUpdateRow[])]);
      const loaded: Row[] = await Promise.all(
        manifest.map(async (entry) => {
          let maintenance: MaintenanceInfo | undefined;
          try {
            const app: AppManifest = await api.registryApp(entry.appId);
            maintenance = app.maintenance;
          } catch {
            /* registry 不可达 → 该行维护数据缺失，操作降级提示 */
          }
          return { entry, maintenance, update: updates.find((u) => u.appId === entry.appId) };
        }),
      );
      setRows(loaded);
      setLoadErr(false);
    } catch {
      setLoadErr(true);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const openRisk = (op: "REMOVE" | "REINSTALL", appId: string) => {
    const paths = buildRemovePaths(rows?.find((r) => r.entry.appId === appId)?.maintenance);
    dispatch({
      type: "M_RISK_OPEN",
      op,
      appId,
      backupFirst: paths.backupFirst,
      riskText: paths.riskText,
      dataPaths: paths.dataPaths,
    });
  };

  const runRemove = async (attempt: RemoveAttempt) => {
    setBusy(true);
    try {
      const res = await api.manageRemove({
        appId: attempt.appId,
        residualPaths: attempt.residualPaths,
        dataPaths: attempt.dataPaths,
        steps: attempt.steps,
        backup: attempt.backup,
      });
      dispatch({
        type: "M_REMOVE_DONE",
        ...(res.backupDir ? { backupDir: res.backupDir } : {}),
        removed: res.removed,
        skipped: res.skipped,
      });
    } catch (e) {
      // 机器码优先（Rust CommandError.code）；英文原文只当「技术细节」，不当人话
      const code = errorCodeOf(e, "MANAGE_DELETE_FAILED");
      const human = e instanceof Error ? e.message : String(e);
      dispatch({ type: "M_ERROR", code, messageKey: manageErrorKey(code, human) });
    } finally {
      setBusy(false);
      void reload();
    }
  };

  const confirmRisk = async () => {
    if (!m.risk || !canConfirmRisk(m.risk) || busy) return;
    const row = rows?.find((r) => r.entry.appId === m.risk!.appId);
    const paths = buildRemovePaths(row?.maintenance);
    if (paths.dataPaths.length === 0 && paths.residualPaths.length === 0 && paths.steps.length === 0) {
      // registry 没有维护清单 → 不允许删除（硬规则④：无清单不执行）
      dispatch({ type: "M_ERROR", code: "MANAGE_NO_MANIFEST", messageKey: "s8.errNoManifest" });
      return;
    }
    dispatch({ type: "M_RISK_CONFIRM" });
    const attempt: RemoveAttempt = {
      appId: m.risk.appId,
      residualPaths: paths.residualPaths,
      dataPaths: paths.dataPaths,
      steps: paths.steps,
      backup: m.backup,
    };
    attemptRef.current = attempt;
    await runRemove(attempt);
  };

  /** 「重试」：真正重发一次删除（旧版只回列表，没重做） */
  const retryRemove = async () => {
    const a = attemptRef.current;
    if (!a || busy) return;
    dispatch({ type: "M_RETRY" });
    await runRemove(a);
  };

  // ---------- DONE / ERROR 全屏态 ----------
  if (m.status === "DONE") {
    const reinstall = m.op === "REINSTALL";
    return (
      <div className="mx-auto flex h-full max-w-xl flex-col gap-4">
        <header className="flex items-center justify-between">
          <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s8.doneTitle")}</h1>
          <button type="button" onClick={() => dispatch({ type: "M_CLOSE" })} className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green">
            {t("net.back")}
          </button>
        </header>
        <div className="panel space-y-2 text-sm">
          {m.removed.length > 0 && (
            <p className="text-aibox-text1">
              {t("s8.doneRemoved", { count: m.removed.length })}
            </p>
          )}
          {m.backupDir && <p className="font-mono text-xs text-aibox-green">{t("s8.doneBackup")}: {m.backupDir}</p>}
          {(() => {
            // skipped 同时承载"本就未找到"与"删除失败"：必须分开呈现，
            // 否则用户会把失败误读为"没有这项数据"（数据安全语义）。
            const notFound = m.skipped.filter((s) => s.reason === "not found").length;
            const failed = m.skipped.length - notFound;
            return (
              <>
                {notFound > 0 && (
                  <p className="text-aibox-text2">{t("s8.doneSkipped", { count: notFound })}</p>
                )}
                {failed > 0 && (
                  <p className="text-aibox-danger">
                    {t("s8.doneFailed", { count: failed })}
                  </p>
                )}
              </>
            );
          })()}
        </div>
        <div className="flex gap-3">
          {reinstall && m.appId && (
            <button type="button" className="btn-cta" onClick={() => { const id = m.appId; if (!id) return; dispatch({ type: "M_CLOSE" }); onGotoInstall(id); }}>
              {t("s8.goInstall")}
            </button>
          )}
          <button type="button" className="rounded-[4px] border border-aibox-line px-4 py-2 text-sm text-aibox-cream hover:border-aibox-green" onClick={() => dispatch({ type: "M_CLOSE" })}>
            {t("s8.close")}
          </button>
        </div>
      </div>
    );
  }

  if (m.status === "ERROR") {
    return (
      <div className="mx-auto flex h-full max-w-xl flex-col gap-4">
        <header className="flex items-center justify-between">
          <h1 className="font-display text-xl font-bold text-aibox-danger">{t("s8.errTitle")}</h1>
          <button type="button" onClick={() => dispatch({ type: "M_CLOSE" })} className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green">
            {t("net.back")}
          </button>
        </header>
        <div className="panel text-sm text-aibox-text1">
          <p>{t(m.error?.messageKey ?? "s8.errRemove")}</p>
          {m.error?.code && (
            <details className="mt-2 font-mono text-[11px] text-aibox-text2">
              <summary className="cursor-pointer">{t("s8.techDetail")}</summary>
              <p className="mt-1 break-all">{m.error.code}</p>
            </details>
          )}
        </div>
        <div className="flex gap-3">
          {/* 重试只在“重试真能生效”时给：MANAGE_NO_MANIFEST 这类按定义不会成功 */}
          {m.error && isRetryableManageError(m.error.code) && attemptRef.current && (
            <button type="button" className="btn-cta" onClick={() => void retryRemove()} disabled={busy}>
              {busy ? t("s8.working") : t("s8.retry")}
            </button>
          )}
          <button type="button" className="rounded-[4px] border border-aibox-line px-4 py-2 text-sm text-aibox-cream hover:border-aibox-green" onClick={() => dispatch({ type: "M_CLOSE" })}>
            {t("s8.close")}
          </button>
        </div>
      </div>
    );
  }

  const risk = m.risk;

  // ---------- 风险卡（破坏性操作确认） ----------
  if (risk) {
    return (
      <div className="mx-auto flex h-full max-w-xl flex-col gap-4">
        <header className="flex items-center justify-between">
          <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s8.riskTitle")}</h1>
          <button type="button" onClick={() => dispatch({ type: "M_RISK_CANCEL" })} className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green">
            {t("btn.cancel")}
          </button>
        </header>
        <div className="panel space-y-3 border-aibox-danger/60 text-sm">
          <p className="text-aibox-cream">{risk.riskText ?? t("s8.riskDefault")}</p>
          {risk.dataPaths.length > 0 && (
            <div>
              <p className="mb-1 text-xs text-aibox-text2">{t("s8.riskPaths")}</p>
              <ul className="space-y-1">
                {risk.dataPaths.map((p) => (
                  <li key={p} className="break-all font-mono text-xs text-aibox-text1">{p}</li>
                ))}
              </ul>
            </div>
          )}
          <label className="flex items-start gap-2">
            <input
              type="checkbox"
              className="mt-1"
              checked={m.backup}
              onChange={(e) => dispatch({ type: "M_RISK_BACKUP", checked: e.target.checked })}
            />
            <span className="text-aibox-text1">{t("s8.backupLabel")}</span>
          </label>
          {!m.backup && (
            <label className="flex items-start gap-2 border-l-2 border-aibox-danger pl-3">
              <input
                type="checkbox"
                className="mt-1"
                checked={risk.noBackupAck}
                onChange={(e) => dispatch({ type: "M_RISK_NO_BACKUP_ACK", acked: e.target.checked })}
              />
              <span className="text-aibox-danger">{t("s8.noBackupAck")}</span>
            </label>
          )}
          <div className="flex gap-3 pt-1">
            <button
              type="button"
              disabled={!canConfirmRisk(risk) || busy}
              onClick={() => void confirmRisk()}
              className="rounded-[4px] bg-aibox-danger px-4 py-2 text-sm font-semibold text-white disabled:opacity-40"
            >
              {busy ? t("s8.working") : risk.op === "REINSTALL" ? t("s8.confirmReinstall") : t("s8.confirmUninstall")}
            </button>
            <button type="button" onClick={() => dispatch({ type: "M_RISK_CANCEL" })} className="rounded-[4px] border border-aibox-line px-4 py-2 text-sm text-aibox-cream hover:border-aibox-green">
              {t("btn.cancel")}
            </button>
          </div>
        </div>
      </div>
    );
  }

  // ---------- 列表态 ----------
  return (
    <div className="mx-auto flex h-full max-w-2xl flex-col gap-4">
      <header className="flex items-center justify-between">
        <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s8.title")}</h1>
        <button type="button" onClick={onClose} className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green">
          {t("net.back")}
        </button>
      </header>
      <p className="text-sm text-aibox-text2">{t("s8.lede")}</p>

      {loadErr && (
        <div className="panel flex items-center justify-between gap-3 text-sm text-aibox-danger">
          <span>{t("s8.loadErr")}</span>
          <button
            type="button"
            onClick={() => void reload()}
            className="shrink-0 rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
          >
            {t("s8.reload")}
          </button>
        </div>
      )}
      {rows && rows.length === 0 && !loadErr && (
        <div className="border border-dashed border-aibox-line p-6 text-sm text-aibox-text2" style={{ borderRadius: 4 }}>
          {t("s8.empty")}
        </div>
      )}

      <ul className="space-y-3">
        {(rows ?? []).map(({ entry, maintenance, update }) => {
          const hasMaint = !!maintenance;
          return (
            <li key={entry.appId} className="panel flex flex-wrap items-center justify-between gap-3">
              <div className="min-w-0">
                <p className="font-display font-semibold text-aibox-cream">{entry.appId}</p>
                <p className="font-mono text-xs text-aibox-text2">
                  {t("s8.installed")}: {entry.version ?? "?"}
                  {update?.hasUpdate && update.latest && (
                    <span className="ml-2 text-aibox-info">{t("s8.updatable", { version: update.latest })}</span>
                  )}
                  {!update?.hasUpdate && update && <span className="ml-2 text-aibox-text2">{t("s8.noUpdate")}</span>}
                </p>
              </div>
              <div className="flex flex-wrap gap-2">
                <button
                  type="button"
                  onClick={() => void reload()}
                  className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
                >
                  {t("s8.checkUpdate")}
                </button>
                {/* 禁用按钮不会派发鼠标事件 → title 悬浮提示在 Chrome/Firefox 里根本不出现，
                    所以禁用原因必须写成**可见小字**，否则小白只看到一个灰按钮。 */}
                <span className="flex flex-col items-end gap-1">
                  <span className="flex flex-wrap gap-2">
                    <button
                      type="button"
                      disabled={!update?.hasUpdate}
                      onClick={() => { dispatch({ type: "M_CLOSE" }); onGotoInstall(entry.appId); }}
                      className="rounded-[4px] bg-aibox-green px-3 py-1.5 font-mono text-xs font-semibold text-aibox-bg disabled:opacity-40"
                    >
                      {t("s8.upgrade")}
                    </button>
                    <button
                      type="button"
                      disabled={!hasMaint}
                      onClick={() => openRisk("REMOVE", entry.appId)}
                      className="rounded-[4px] border border-aibox-danger/70 px-3 py-1.5 font-mono text-xs text-aibox-danger hover:border-aibox-danger disabled:opacity-40"
                    >
                      {t("s8.uninstall")}
                    </button>
                    <button
                      type="button"
                      disabled={!hasMaint}
                      onClick={() => openRisk("REINSTALL", entry.appId)}
                      className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green disabled:opacity-40"
                    >
                      {t("s8.reinstall")}
                    </button>
                  </span>
                  {!update?.hasUpdate && (
                    <span className="font-mono text-[10px] text-aibox-text2">{t("s8.noUpdateHint")}</span>
                  )}
                  {!hasMaint && (
                    <span className="font-mono text-[10px] text-aibox-text2">{t("s8.noMaintHint")}</span>
                  )}
                </span>
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}
