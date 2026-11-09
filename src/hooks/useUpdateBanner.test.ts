/**
 * 升级黄条纯逻辑：会话内只弹一次（dismiss 后 5min 缓存重查也不再弹）+ 官网下载页。
 */
import { describe, expect, it } from "vitest";
import {
  bannerReduce,
  bannerVisible,
  createBannerState,
  OFFICIAL_DOWNLOAD_URL,
} from "./useUpdateBanner";

describe("bannerReduce / 会话内只弹一次", () => {
  it("有新版 → 可见", () => {
    let s = createBannerState();
    s = bannerReduce(s, { type: "CHECK_RESULT", info: { hasUpdate: true, version: "0.1.0" } });
    expect(bannerVisible(s)).toBe(true);
    expect(s.info?.version).toBe("0.1.0");
  });

  it("无新版 → 零 UI", () => {
    let s = createBannerState();
    s = bannerReduce(s, { type: "CHECK_RESULT", info: { hasUpdate: false } });
    expect(bannerVisible(s)).toBe(false);
  });

  it("关闭后再次 CHECK_RESULT（5min 缓存重查）→ 本会话不再弹", () => {
    let s = createBannerState();
    s = bannerReduce(s, { type: "CHECK_RESULT", info: { hasUpdate: true, version: "0.1.0" } });
    s = bannerReduce(s, { type: "DISMISS" });
    expect(bannerVisible(s)).toBe(false);
    // 第二次 update_check 结果进来（真分支启动时可能发生）
    s = bannerReduce(s, { type: "CHECK_RESULT", info: { hasUpdate: true, version: "0.1.1" } });
    expect(bannerVisible(s)).toBe(false);
    expect(s.dismissed).toBe(true);
  });

  it("DISMISS 是幂等的", () => {
    let s = createBannerState();
    s = bannerReduce(s, { type: "DISMISS" });
    s = bannerReduce(s, { type: "DISMISS" });
    expect(s.dismissed).toBe(true);
  });
});

describe("黄条跳转", () => {
  it("固定官网下载页（不自替换）", () => {
    expect(OFFICIAL_DOWNLOAD_URL).toBe("https://aibox.im/download/");
  });
});
