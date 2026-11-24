/** S8: installed app updates, backup access, and read-only uninstall/reinstall guidance. */
import { KeyBatchPanel } from "../components/KeyBatchPanel";
import { RestorePanel } from "../components/RestorePanel";
import { PreservedPanel } from "../components/PreservedPanel";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { api, type AppUpdateRow, type ManifestEntry } from "../net/client";

interface Props {
  onClose: () => void;
  /** Open the standard install flow for an upgrade. */
  onGotoInstall: (appId: string) => void;
  focus?: "upgrades" | null;
  onUpgradeAll?: (appIds: string[]) => void;
  upgradeBackup?: { appId: string; path: string; at: number } | null;
}

interface Row {
  entry: ManifestEntry;
  uninstallSteps: string[];
  update?: AppUpdateRow;
}

export default function S8Manage({ onClose, onGotoInstall, focus, onUpgradeAll, upgradeBackup }: Props) {
  const { t } = useTranslation();
  const [rows, setRows] = useState<Row[] | null>(null);
  const [loadErr, setLoadErr] = useState(false);
  const [revealErr, setRevealErr] = useState(false);

  const reload = useCallback(async () => {
    try {
      const [manifest, updates] = await Promise.all([api.manageManifestList(), api.checkAppUpdates().catch(() => [] as AppUpdateRow[])]);
      const loaded = await Promise.all(
        manifest.map(async (entry) => {
          let uninstallSteps: string[] = [];
          try {
            const app = await api.registryApp(entry.appId);
            const steps = app.maintenance?.uninstall?.steps;
            if (Array.isArray(steps)) {
              uninstallSteps = steps.filter((step) => typeof step === "string" && step.trim().length > 0);
            }
          } catch {
            // Registry unavailable: the guide still points to official instructions.
          }
          return { entry, uninstallSteps, update: updates.find((u) => u.appId === entry.appId) };
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

  const updatableRows = (rows ?? [])
    .filter((r) => r.update?.hasUpdate)
    .sort((a, b) => (a.update?.latest ?? "").localeCompare(b.update?.latest ?? ""));
  const orderedRows = focus === "upgrades"
    ? [...updatableRows, ...(rows ?? []).filter((r) => !r.update?.hasUpdate)]
    : (rows ?? []);

  return (
    <div className="mx-auto flex h-full max-w-2xl flex-col gap-4">
      <header className="flex items-center justify-between">
        <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s8.title")}</h1>
        <button type="button" onClick={onClose} className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green">
          {t("net.back")}
        </button>
      </header>
      <p className="text-sm text-aibox-text2">{t("s8.lede")}</p>

      {upgradeBackup && (
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-[6px] border border-aibox-line bg-aibox-raised px-4 py-3">
          <span className="min-w-0 flex-1 text-xs leading-relaxed text-aibox-text2">
            {t("s8.backupLast", { appId: upgradeBackup.appId, path: upgradeBackup.path })}
          </span>
          <button
            type="button"
            onClick={() => {
              setRevealErr(false);
              void api.revealDir(upgradeBackup.path).catch(() => setRevealErr(true));
            }}
            className="shrink-0 rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-[10px] text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
          >
            {t("s7.openFolder")}
          </button>
          {revealErr && <p className="w-full text-[11px] text-aibox-danger">{t("s8.restoreRevealFail")}</p>}
        </div>
      )}

      {focus === "upgrades" && updatableRows.length > 0 && (
        <div className="flex flex-wrap items-center justify-between gap-3 rounded-[6px] border border-aibox-green/60 bg-aibox-green/10 px-4 py-3">
          <span className="text-sm text-aibox-cream">{t("s8.upgradeFocus", { count: updatableRows.length })}</span>
          <span className="flex gap-2">
            {updatableRows.length > 1 && onUpgradeAll && (
              <button
                type="button"
                onClick={() => onUpgradeAll(updatableRows.map((r) => r.entry.appId))}
                data-testid="s8-upgrade-all"
                className="rounded-[4px] border border-aibox-green px-4 py-2 font-mono text-xs text-aibox-green hover:bg-aibox-green/10"
              >
                {t("s8.upgradeAll")}
              </button>
            )}
            <button
              type="button"
              onClick={() => onGotoInstall(updatableRows[0].entry.appId)}
              className="rounded-[4px] bg-aibox-green px-4 py-2 font-display text-sm font-semibold text-aibox-bg"
            >
              {t("s8.upgradeNext")}
            </button>
          </span>
        </div>
      )}

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
        <div className="rounded-[4px] border border-dashed border-aibox-line p-6 text-sm text-aibox-text2">
          {t("s8.empty")}
        </div>
      )}
      {rows && rows.length > 0 && (
        <KeyBatchPanel installed={rows.map((r) => ({ appId: r.entry.appId }))} />
      )}
      <RestorePanel />
      <PreservedPanel />

      <ul className="space-y-3">
        {orderedRows.map(({ entry, uninstallSteps, update }) => (
          <li key={entry.appId} className="panel space-y-3">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <div className="min-w-0">
                <p className="font-display font-semibold text-aibox-cream">{entry.appId}</p>
                <p className="font-mono text-xs text-aibox-text2">
                  {t("s8.installed")}: {entry.version ?? t("s7.versionUnavailable")}
                  {update?.hasUpdate && update.latest && (
                    <span className="ml-2 text-aibox-info">{t("s8.updatable", { version: update.latest })}</span>
                  )}
                  {!update?.hasUpdate && update?.installed && update.latest && <span className="ml-2 text-aibox-text2">{t("s8.noUpdate")}</span>}
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
                <button
                  type="button"
                  disabled={!update?.hasUpdate}
                  onClick={() => onGotoInstall(entry.appId)}
                  className="rounded-[4px] bg-aibox-green px-3 py-1.5 font-mono text-xs font-semibold text-aibox-bg disabled:opacity-40"
                >
                  {t("s8.upgrade")}
                </button>
              </div>
              {!update?.hasUpdate && update?.installed && update.latest && <span className="w-full text-right font-mono text-[10px] text-aibox-text2">{t("s8.noUpdateHint")}</span>}
            </div>
            <details className="border-t border-aibox-line pt-2 text-xs leading-relaxed text-aibox-text2">
              <summary className="cursor-pointer text-aibox-cream">{t("s8.manualGuideTitle")}</summary>
              <p className="mt-2">{t("s8.manualUninstall")}</p>
              {uninstallSteps.length > 0 ? (
                <>
                  <p className="mt-2">{t("s8.manualPublishedSteps")}</p>
                  <ol className="list-decimal space-y-1 pl-5">
                    {uninstallSteps.map((step, index) => (
                      <li key={index} className="whitespace-pre-wrap break-words">{step}</li>
                    ))}
                  </ol>
                </>
              ) : (
                <p className="mt-2">{t("s8.manualNoSteps")}</p>
              )}
              <p className="mt-2">{t("s8.manualReinstall")}</p>
              <p className="mt-2">{t("s8.manualRecord")}</p>
            </details>
          </li>
        ))}
      </ul>
    </div>
  );
}
