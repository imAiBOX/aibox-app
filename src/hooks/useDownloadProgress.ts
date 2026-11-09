/**
 * 订阅 download_progress 事件（mock 或 Rust emit）→ 当前下载进度。
 * 联调时 Rust 事件自动流入，无需改调用方。
 */
import { useEffect, useState } from "react";
import { onEvent, type AiboxEvent } from "../net/client";

export interface DownloadProgress {
  jobId: string;
  done: number;
  total: number;
  channel: string;
  /** 0-100 */
  pct: number;
}

const IDLE: DownloadProgress = { jobId: "", done: 0, total: 0, channel: "", pct: 0 };

export function useDownloadProgress(): DownloadProgress {
  const [p, setP] = useState<DownloadProgress>(IDLE);

  useEffect(() => {
    const off = onEvent((e: AiboxEvent) => {
      if (e.event !== "download_progress") return;
      const { jobId, done, total, channel } = e.payload;
      setP({
        jobId,
        done,
        total,
        channel,
        pct: total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0,
      });
    });
    return off;
  }, []);

  return p;
}
