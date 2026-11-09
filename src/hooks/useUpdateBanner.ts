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

/** 官网下载页（跳官网，不用 releases 直链） */
export const OFFICIAL_DOWNLOAD_URL = "https://aibox.im/download/";

export interface BannerState {
  info: UpdateInfo | null;
  /** 本会话内已关闭（不落盘 → 本次启动不再弹） */
  dismissed: boolean;
}

export type BannerAction =
  | { type: "CHECK_RESULT"; info: UpdateInfo }
  | { type: "DISMISS" };

/** 纯逻辑：CHECK_RESULT 在 dismissed 后忽略（会话内只弹一次） */
export function bannerReduce(s: BannerState, a: BannerAction): BannerState {
  switch (a.type) {
    case "CHECK_RESULT":
      // 已关闭 → 后续 update_check 结果（5min 缓存重查）不再展示
      return s.dismissed ? s : { ...s, info: a.info };
    case "DISMISS":
      return { ...s, dismissed: true };
    default:
      return s;
  }
}

export function createBannerState(): BannerState {
  return { info: null, dismissed: false };
}

export function bannerVisible(s: BannerState): boolean {
  return Boolean(s.info?.hasUpdate) && !s.dismissed;
}

export interface UpdateBannerState {
  visible: boolean;
  version?: string;
  url: string;
  dismiss: () => void;
}

export function useUpdateBanner(): UpdateBannerState {
  const [state, rawDispatch] = useReducer(bannerReduce, undefined, createBannerState);

  useEffect(() => {
    let alive = true;
    api
      .updateCheck()
      .then((r) => alive && rawDispatch({ type: "CHECK_RESULT", info: r }))
      .catch(() => alive && rawDispatch({ type: "CHECK_RESULT", info: { hasUpdate: false } }));
    return () => {
      alive = false;
    };
  }, []);

  const dismiss = useCallback(() => rawDispatch({ type: "DISMISS" }), []);

  return {
    visible: bannerVisible(state),
    version: state.info?.version,
    url: OFFICIAL_DOWNLOAD_URL,
    dismiss,
  };
}
