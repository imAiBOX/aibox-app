/**
 * events 测试：toast 3s 自动消失（注入 timer）、log_line 4KB 截断、事件桥 noop 分支。
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  createLogBuffer,
  createToastController,
  feedLogFromEvents,
  feedToastFromEvents,
  LOG_CAP_BYTES,
  startTauriEvents,
  TOAST_MS,
} from "./events";
import { emitEvent } from "./client";

afterEach(() => {
  vi.useRealTimers();
});

describe("通道 toast（3s 自动消失）", () => {
  it("默认 3000ms", () => {
    expect(TOAST_MS).toBe(3000);
  });

  it("show → 订阅者收到文本；3s 后自动置 null", () => {
    vi.useFakeTimers();
    const toast = createToastController();
    const seen: (string | null)[] = [];
    toast.subscribe((t) => seen.push(t));
    toast.show("GitHub 超时 → npmmirror");
    expect(seen).toContain("GitHub 超时 → npmmirror");
    vi.advanceTimersByTime(2999);
    expect(seen[seen.length - 1]).toBe("GitHub 超时 → npmmirror");
    vi.advanceTimersByTime(1);
    expect(seen[seen.length - 1]).toBeNull();
  });

  it("重复 show 重置计时（后一条完整展示 3s）", () => {
    vi.useFakeTimers();
    const toast = createToastController();
    toast.show("one");
    vi.advanceTimersByTime(2000);
    toast.show("two");
    vi.advanceTimersByTime(2999);
    // one 的原始计时被清；two 还在
    let latest: string | null = null;
    toast.subscribe((t) => (latest = t));
    expect(latest).toBe("two");
    vi.advanceTimersByTime(1);
    expect(latest).toBeNull();
  });

  it("hide 立即消失", () => {
    vi.useFakeTimers();
    const toast = createToastController();
    toast.show("x");
    toast.hide();
    let latest: string | null = "init";
    toast.subscribe((t) => (latest = t));
    expect(latest).toBeNull();
    vi.advanceTimersByTime(5000);
    expect(latest).toBeNull();
  });

  it("channel_switched 事件喂入 toast（统一事件总线）", () => {
    const toast = createToastController();
    const off = feedToastFromEvents(toast, (p) => `${p.from}→${p.to}`);
    emitEvent({
      event: "channel_switched",
      payload: { from: "github.com", to: "npmmirror", reason: "timeout" },
    });
    let latest: string | null = null;
    toast.subscribe((t) => (latest = t));
    expect(latest).toBe("github.com→npmmirror");
    off();
  });
});

describe("日志环形缓冲（4KB 截断）", () => {
  it("默认容量 4096 字节（截断 4KB）", () => {
    expect(LOG_CAP_BYTES).toBe(4096);
  });

  it("超限丢最旧行", () => {
    const buf = createLogBuffer(100); // 10 行 ×11B(10字符+换行)
    for (let i = 0; i < 20; i++) buf.push(`line-${i}`.padEnd(10, "x"));
    expect(buf.bytes()).toBeLessThanOrEqual(100);
    const lines = buf.lines();
    expect(lines.length).toBeLessThan(20);
    expect(lines[0]).toContain("line-"); // 保留的是较新的
    expect(lines[lines.length - 1]).toBe("line-19".padEnd(10, "x"));
  });

  it("单行超限 → 截尾保留", () => {
    const buf = createLogBuffer(50);
    buf.push("y".repeat(500));
    expect(buf.bytes()).toBeLessThanOrEqual(50);
    expect(buf.lines()).toHaveLength(1);
  });

  it("log_line 事件喂入", () => {
    const buf = createLogBuffer();
    const off = feedLogFromEvents(buf);
    emitEvent({ event: "log_line", payload: { text: "hello" } });
    expect(buf.lines()).toEqual(["hello"]);
    off();
    emitEvent({ event: "log_line", payload: { text: "after" } });
    expect(buf.lines()).toEqual(["hello"]);
  });

  it("clear 清空", () => {
    const buf = createLogBuffer();
    buf.push("a");
    buf.clear();
    expect(buf.lines()).toEqual([]);
    expect(buf.bytes()).toBe(0);
  });
});

describe("startTauriEvents（mock/浏览器 noop）", () => {
  it("返回退订函数且不抛", () => {
    const stop = startTauriEvents();
    expect(typeof stop).toBe("function");
    expect(() => stop()).not.toThrow();
  });
});
