/**
 * 订阅 download_progress 事件（mock 或 Rust emit）→ 当前下载进度。
 * 联调时 Rust 事件自动流入，无需改调用方。
 *
 * **顺带算速度与剩余时间**（不用改 Rust）：
 * 事件本来就带字节数 `done`/`total`，前端保留上次采样点即可算滚动速度。
 * Rust 侧 `DownloadProgress` 结构体里虽有 `segment/segments/resumed`，
 * 但 emit 时被丢弃（只传 jobId/done/total/channel）→ 前端只能自己估。
 */
import { useEffect, useRef, useState } from "react";
import { onEvent, type AiboxEvent } from "../net/client";

export interface DownloadProgress {
  jobId: string;
  done: number;
  total: number;
  channel: string;
  /** 0-100 */
  pct: number;
  /** 滚动速度（字节/秒）；样本不足时为 null */
  bps: number | null;
  /** 预计剩余秒数；速度未知或已下完时为 null */
  etaSec: number | null;
}

const IDLE: DownloadProgress = {
  jobId: "",
  done: 0,
  total: 0,
  channel: "",
  pct: 0,
  bps: null,
  etaSec: null,
};

export function useDownloadProgress(): DownloadProgress {
  const [p, setP] = useState<DownloadProgress>(IDLE);
  /** 上次采样点（用于算滚动速度） */
  const last = useRef<{ done: number; ts: number; jobId: string } | null>(null);

  useEffect(() => {
    const off = onEvent((e: AiboxEvent) => {
      if (e.event !== "download_progress") return;
      const { jobId, done, total, channel } = e.payload;
      const now = Date.now();
      const prev = last.current;
      // 换任务 → 重置采样（否则会用上个任务的字节算速度）
      if (!prev || prev.jobId !== jobId) {
        last.current = { done, ts: now, jobId };
        setP({
          jobId,
          done,
          total,
          channel,
          pct: total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0,
          bps: null,
          etaSec: null,
        });
        return;
      }
      let bps: number | null = null;
      const dt = now - prev.ts;
      // 采样窗口内累计：字节增量 / 时间增量（≥4MB 或 ≥4s 才更新一次，避免抖动）
      if (dt >= 4000 && done > prev.done) {
        bps = ((done - prev.done) / dt) * 1000;
        last.current = { done, ts: now, jobId };
      }
      const eta = bps && bps > 0 && total > done ? Math.round((total - done) / bps) : null;
      setP({
        jobId,
        done,
        total,
        channel,
        pct: total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0,
        bps,
        etaSec: eta,
      });
    });
    return off;
  }, []);

  return p;
}

/** 字节数 → 人话（120.4 MB / 1.2 GB） */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  return `${v.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

/** 秒数 → 人话（1 分 20 秒 / 45 秒） */
export function formatEta(sec: number | null): string | null {
  if (sec === null || !Number.isFinite(sec) || sec < 0) return null;
  const s = Math.round(sec);
  if (s < 60) return `${s} 秒`;
  const m = Math.floor(s / 60);
  const rest = s % 60;
  return rest === 0 ? `${m} 分` : `${m} 分 ${rest} 秒`;
}
