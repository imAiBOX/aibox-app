/**
 * 空闲预下载纯逻辑：待预下载队列（hasUpdate 去重保序）+ IDLE 门控。
 */
import { describe, expect, it } from "vitest";
import { prefetchGate, prefetchQueue } from "./prefetch";
import type { AppUpdateRow } from "../net/client";

const row = (appId: string, hasUpdate: boolean): AppUpdateRow => ({ appId, hasUpdate });

describe("prefetchQueue", () => {
  it("只取有新版的 appId，去重且保序", () => {
    const rows = [
      row("a", true),
      row("b", false),
      row("c", true),
      row("a", true), // 重复行去重
      row("", true), // 空 id 丢弃
    ];
    expect(prefetchQueue(rows)).toEqual(["a", "c"]);
  });

  it("无新版 / 空结果 → 空队列（不触发预下载）", () => {
    expect(prefetchQueue([row("a", false)])).toEqual([]);
    expect(prefetchQueue([])).toEqual([]);
  });
});

describe("prefetchGate", () => {
  it("仅常驻空闲 IDLE 触发；安装/诊断等进行中一律不抢带宽", () => {
    expect(prefetchGate("IDLE")).toBe(true);
    for (const s of ["SCAN", "PLAN", "CONFIRM", "EXECUTE", "CONFIG", "VERIFY", "DONE", "ERROR"]) {
      expect(prefetchGate(s)).toBe(false);
    }
  });
});
