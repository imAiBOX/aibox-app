/**
 * 本体升级黄条：启动 update_check（5min 缓存，真分支）→ 有新版显示黄条，
 * 「下载」固定跳官网下载页 https://aibox.im/download/（不强制、不自替换）。
 * 关闭 = 本会话内不再弹（内存态，不持久化）；无新版零 UI。
 * mock 模式保持场景开关（setMockScenario('update_available')）。
 *
 * 状态逻辑抽成纯 reducer（bannerReduce）→ 无 DOM 单测覆盖「会话内只弹一次」。
 */
import { useCallback, useEffect, useReducer } from "react";
import { api, type UpdateInfo } from "../net/client";
import { isFirstLaunch } from "../state/firstRun";

/**
 * 官网下载页（跳官网，不用 releases 直链）。
 * 根路径 `/download/` 已有语言兼容跳转（2026-11-20 补：原本是 404），
 * 但界面上的链接会用 `siteUrl('/download/', lang)` 直接指到当前语言版。
 */
export const OFFICIAL_DOWNLOAD_URL = "https://aibox.im/download/";

export interface BannerState {
  info: UpdateInfo | null;
  /** 已关闭（按版本持久化 → 同一版本不再每次启动都弹） */
  dismissed: boolean;
}

/** 「稍后」的持久化键（记被关闭的版本号；同版本不再弹） */
const DISMISS_KEY = "aibox.update.dismissed";

/** 读出被「稍后」关闭的版本（无 → null） */
export function readDismissedVersion(): string | null {
  try {
    if (typeof localStorage === "undefined") return null;
    return localStorage.getItem(DISMISS_KEY);
  } catch {
    return null;
  }
}

/** 记住「稍后」关闭的版本（下次启动同版本不再弹） */
export function rememberDismissedVersion(v: string): void {
  try {
    if (typeof localStorage === "undefined") return;
    localStorage.setItem(DISMISS_KEY, v);
  } catch {
    /* 隐私模式等 → 忽略（退化成会话内有效） */
  }
}

export type BannerAction =
  | { type: "CHECK_RESULT"; info: UpdateInfo }
  | { type: "DISMISS"; version?: string };

/** 纯逻辑：CHECK_RESULT 在 dismissed 后忽略（会话内只弹一次） */
export function bannerReduce(s: BannerState, a: BannerAction): BannerState {
  switch (a.type) {
    case "CHECK_RESULT":
      // 已关闭 → 后续 update_check 结果（5min 缓存重查）不再展示
      if (s.dismissed) return s;
      // **同版本被「稍后」过** → 本次启动也不再展示（持久化，见 rememberDismissedVersion）
      const dismissedVersion = readDismissedVersion();
      if (
        dismissedVersion &&
        a.info.hasUpdate &&
        a.info.version &&
        dismissedVersion === String(a.info.version)
      ) {
        return { ...s, dismissed: true };
      }
      return { ...s, info: a.info };
    case "DISMISS":
      // 持久化：同一版本不再每次启动都弹（旧版只内存态 → 用户点了「稍后」下次照弹）
      if (a.version) rememberDismissedVersion(a.version);
      return { ...s, dismissed: true };
    default:
      return s;
  }
}

export function createBannerState(): BannerState {
  return { info: null, dismissed: false };
}

export function bannerVisible(s: BannerState, firstLaunch = false): boolean {
  // **首次启动不弹本体更新条**：刚下载安装的用户第一眼看到「发现新版本」会懵
  // （「我不是刚装的吗？是不是装错了？」）→ 第一次只留在托盘菜单里，不占首屏。
  if (firstLaunch) return false;
  return Boolean(s.info?.hasUpdate) && !s.dismissed;
}

export interface UpdateBannerState {
  visible: boolean;
  version?: string;
  url: string;
  /** 原始检查结果（托盘联动用：不受 dismissed 影响） */
  info: UpdateInfo | null;
  dismiss: (version?: string) => void;
  /** 重新检查（5min 缓存；托盘「检查更新」/ 常驻周期用） */
  recheck: () => void;
}

export function useUpdateBanner(): UpdateBannerState {
  const [state, rawDispatch] = useReducer(bannerReduce, undefined, createBannerState);

  const recheck = useCallback(() => {
    api
      .updateCheck()
      .then((r) => rawDispatch({ type: "CHECK_RESULT", info: r }))
      .catch(() => rawDispatch({ type: "CHECK_RESULT", info: { hasUpdate: false } }));
  }, []);

  useEffect(() => {
    recheck();
  }, [recheck]);

  // 「稍后」带上版本号 → 持久化（同一版本不再每次启动都弹）
  const dismiss = useCallback(
    (version?: string) => rawDispatch({ type: "DISMISS", ...(version ? { version } : {}) }),
    [],
  );

  return {
    visible: bannerVisible(state, isFirstLaunch()),
    version: state.info?.version,
    url: OFFICIAL_DOWNLOAD_URL,
    info: state.info,
    dismiss,
    recheck,
  };
}
