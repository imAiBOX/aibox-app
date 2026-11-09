/**
 * 本体升级黄条：update_check（5min 缓存）→ 有新版显示黄条，
 * 「下载」跳官网（不强制、不自替换）。
 */
import { useCallback, useEffect, useState } from "react";
import { api, type UpdateInfo } from "../net/client";

export interface UpdateBannerState {
  visible: boolean;
  version?: string;
  url?: string;
  dismiss: () => void;
}

export function useUpdateBanner(): UpdateBannerState {
  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    let alive = true;
    api
      .updateCheck()
      .then((r) => alive && setInfo(r))
      .catch(() => alive && setInfo({ hasUpdate: false }));
    return () => {
      alive = false;
    };
  }, []);

  const dismiss = useCallback(() => setDismissed(true), []);

  return {
    visible: Boolean(info?.hasUpdate) && !dismissed,
    version: info?.version,
    url: info?.url ?? "https://aibox.im/download",
    dismiss,
  };
}
