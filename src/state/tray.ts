/**
 * 托盘文案推导（纯函数，可单测）：
 * - 本体有新版 → 首项变「有新版本 vX → 下载」（firstAction=download）+ tooltip 标记
 * - 仅应用可升级 → 首项显示数量（打开主窗口）+ tooltip 标记数量
 * - 都没有 → 「打开 AiBOX」+ 纯 tooltip
 * 文案由 i18n 提供（t 注入）；Rust 侧只负责展示。
 */
import type { TraySync } from "../net/client";

type TFn = (key: string, opts?: Record<string, string | number>) => string;

export interface TrayState {
  hasCoreUpdate: boolean;
  coreVersion?: string;
  appUpdateCount: number;
}

export function buildTrayLabels(t: TFn, s: TrayState): TraySync {
  const firstAction: TraySync["firstAction"] = s.hasCoreUpdate ? "download" : "open";
  const firstLabel = s.hasCoreUpdate
    ? t("tray.firstUpdate", { version: s.coreVersion ?? "?" })
    : s.appUpdateCount > 0
      ? t("tray.appUpdates", { count: s.appUpdateCount })
      : t("tray.open");
  const tooltip = s.hasCoreUpdate
    ? t("tray.tipUpdate")
    : s.appUpdateCount > 0
      ? t("tray.tipApps", { count: s.appUpdateCount })
      : t("tray.tip");
  return {
    tooltip,
    firstLabel,
    firstAction,
    checkLabel: t("tray.check"),
    quitLabel: t("tray.quit"),
  };
}
