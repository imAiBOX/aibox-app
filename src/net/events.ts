/**
 * 事件统一入口：
 * - 真 Tauri：`@tauri-apps/api/event` listen 四事件 → 转发进 client.ts 的 mock 同构总线；
 *   mock 模式不启动监听（mock 已经 globalThis.__aiboxEmit 推送）。
 * - 通道 toast：3s 自动消失（纯控制器，可注入 timer 单测）。
 * - 日志抽屉：log_line 环形缓冲，4KB 截断语义（截断 4KB/任务）。
 */
import { emitEvent, isMock, onEvent, type AiboxEvent } from "./client";

export { onEvent, emitEvent };

const EVENT_NAMES = [
  "download_progress",
  "task_phase",
  "channel_switched",
  "log_line",
  "tray_action",
] as const;

/** 幂等启动真事件桥（App 启动时调一次；mock/非 Tauri 环境为 noop） */
export function startTauriEvents(): () => void {
  if (isMock()) return () => undefined;
  if (typeof globalThis === "undefined" || !("__TAURI_INTERNALS__" in globalThis)) {
    return () => undefined;
  }
  let stopped = false;
  const unlisteners: Array<() => void> = [];
  void (async () => {
    try {
      const { listen } = await import("@tauri-apps/api/event");
      for (const name of EVENT_NAMES) {
        if (stopped) return;
        const un = await listen<Record<string, unknown>>(name, (e) => {
          emitEvent({ event: name, payload: e.payload } as AiboxEvent);
        });
        if (stopped) un();
        else unlisteners.push(un);
      }
    } catch {
      /* 事件桥失败不阻塞 UI（进度/日志降级为空） */
    }
  })();
  return () => {
    stopped = true;
    for (const un of unlisteners) {
      try {
        un();
      } catch {
        /* ignore */
      }
    }
    unlisteners.length = 0;
  };
}

/* ── 通道切换 toast（自动消失 3s） ── */
export const TOAST_MS = 3000;

export interface ToastController {
  /** 展示 toast（重复调用重置计时） */
  show(text: string): void;
  /** 立即隐藏 */
  hide(): void;
  /** 订阅当前文本（null=隐藏）；返回退订 */
  subscribe(fn: (text: string | null) => void): () => void;
}

export function createToastController(
  durationMs: number = TOAST_MS,
  timers: {
    setTimeout: (fn: () => void, ms: number) => unknown;
    clearTimeout: (h: unknown) => void;
  } = { setTimeout: (fn, ms) => setTimeout(fn, ms), clearTimeout: (h) => clearTimeout(h as ReturnType<typeof setTimeout>) },
): ToastController {
  let text: string | null = null;
  let timer: unknown = null;
  const subs = new Set<(t: string | null) => void>();
  const notify = () => {
    for (const fn of [...subs]) fn(text);
  };
  const clear = () => {
    if (timer !== null) {
      timers.clearTimeout(timer);
      timer = null;
    }
  };
  return {
    show(t) {
      text = t;
      clear();
      timer = timers.setTimeout(() => {
        text = null;
        timer = null;
        notify();
      }, durationMs);
      notify();
    },
    hide() {
      clear();
      if (text !== null) {
        text = null;
        notify();
      }
    },
    subscribe(fn) {
      subs.add(fn);
      fn(text);
      return () => subs.delete(fn);
    },
  };
}

/** 全局单例（App 订阅渲染；channel_switched 事件自动喂入） */
export const channelToast = createToastController();

/** 把 channel_switched 事件接入 toast（返回退订）；由 App 调用一次 */
export function feedToastFromEvents(
  toast: ToastController = channelToast,
  format: (p: { from: string; to: string; reason: string }) => string = (p) =>
    `${p.from} → ${p.to}`,
): () => void {
  return onEvent((e) => {
    if (e.event !== "channel_switched") return;
    toast.show(format(e.payload));
  });
}

/* ── 日志环形缓冲（只读抽屉数据源；4KB 截断） ── */
export const LOG_CAP_BYTES = 4096;

export interface LogBuffer {
  push(text: string): void;
  lines(): string[];
  clear(): void;
  /** 当前占用字节（UTF-8 近似：按 code unit 计，够截断语义用） */
  bytes(): number;
}

export function createLogBuffer(capBytes: number = LOG_CAP_BYTES): LogBuffer {
  let lines: string[] = [];
  let size = 0;
  const lineBytes = (s: string) => s.length + 1;
  const trim = () => {
    while (size > capBytes && lines.length > 0) {
      size -= lineBytes(lines[0]);
      lines.shift();
    }
  };
  return {
    push(text) {
      // 单行超限：截尾保留（4KB/任务语义）
      let line = text;
      if (lineBytes(line) > capBytes) line = line.slice(0, capBytes - 1);
      lines.push(line);
      size += lineBytes(line);
      trim();
    },
    lines: () => [...lines],
    clear() {
      lines = [];
      size = 0;
    },
    bytes: () => size,
  };
}

/** 全局日志缓冲 + log_line 接线（返回退订） */
export const appLog = createLogBuffer();

export function feedLogFromEvents(buffer: LogBuffer = appLog): () => void {
  return onEvent((e) => {
    if (e.event === "log_line") buffer.push(e.payload.text);
  });
}
