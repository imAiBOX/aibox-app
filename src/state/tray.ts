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
  /** 目录新上的应用数（待办项之一；缺省 0） */
  newAppCount?: number;
  /** 最近安装的应用（托盘右键直接装） */
  recent?: { id: string; name: string }[];
}

export function buildTrayLabels(t: TFn, s: TrayState): TraySync {
  const newCount = s.newAppCount ?? 0;
  const firstAction: TraySync["firstAction"] = s.hasCoreUpdate ? "download" : "open";
  const firstLabel = s.hasCoreUpdate
    ? t("tray.firstUpdate", { version: s.coreVersion ?? "?" })
    : s.appUpdateCount > 0
      ? t("tray.appUpdates", { count: s.appUpdateCount })
      : newCount > 0
        ? t("tray.newApps", { count: newCount })
        : t("tray.open");
  const tooltip = s.hasCoreUpdate
    ? t("tray.tipUpdate")
    : s.appUpdateCount > 0
      ? t("tray.tipApps", { count: s.appUpdateCount })
      : newCount > 0
        ? t("tray.tipNew", { count: newCount })
        : t("tray.tip");
  // 角标数字 = 待办总数（Windows 托盘图标旁；其他平台忽略）
  const total = s.appUpdateCount + newCount + (s.hasCoreUpdate ? 1 : 0);
  return {
    tooltip,
    firstLabel,
    firstAction,
    checkLabel: t("tray.check"),
    quitLabel: t("tray.quit"),
    badge: total > 0 ? String(total) : "",
    pendingUpdatesLabel: t("tray.pendingUpdates", { count: s.appUpdateCount }),
    pendingUpdatesCount: s.appUpdateCount,
    pendingNewLabel: t("tray.pendingNew", { count: newCount }),
    pendingNewCount: newCount,
    recent: (s.recent ?? []).slice(0, 3).map((r) => ({
      id: r.id,
      label: t("tray.recentInstall", { name: r.name }),
    })),
    pendingCoreLabel: s.coreVersion
      ? t("tray.pendingCore", { version: s.coreVersion })
      : t("tray.pendingCoreNoVersion"),
    pendingCoreCount: s.hasCoreUpdate ? 1 : 0,
  };
}
