/**
 * 托盘菜单状态推导：无更新 / 本体新版 / 应用可升级 → tooltip + 首项文案与动作。
 */
import { describe, expect, it } from "vitest";
import { buildTrayLabels, type TrayState } from "./tray";

/** 测试用 t：key + 插值 → 可读串（不依赖 i18next 运行时） */
function t(key: string, opts?: Record<string, string | number>): string {
  if (!opts) return key;
  return `${key}(${Object.entries(opts)
    .map(([k, v]) => `${k}=${v}`)
    .join(",")})`;
}

const base: TrayState = { hasCoreUpdate: false, appUpdateCount: 0 };

describe("buildTrayLabels", () => {
  it("无更新：首项=打开，tooltip=纯名", () => {
    const s = buildTrayLabels(t, base);
    expect(s.firstAction).toBe("open");
    expect(s.firstLabel).toBe("tray.open");
    expect(s.tooltip).toBe("tray.tip");
    expect(s.checkLabel).toBe("tray.check");
    expect(s.quitLabel).toBe("tray.quit");
  });

  it("本体有新版：首项变下载动作 + tooltip 标记（版本注入）", () => {
    const s = buildTrayLabels(t, {
      hasCoreUpdate: true,
      coreVersion: "v0.1.2",
      appUpdateCount: 3,
    });
    expect(s.firstAction).toBe("download");
    expect(s.firstLabel).toBe("tray.firstUpdate(version=v0.1.2)");
    expect(s.tooltip).toBe("tray.tipUpdate");
  });

  it("本体无新版但应用可升级：首项显示数量（仍开窗口）+ tooltip 数量", () => {
    const s = buildTrayLabels(t, { hasCoreUpdate: false, appUpdateCount: 2 });
    expect(s.firstAction).toBe("open");
    expect(s.firstLabel).toBe("tray.appUpdates(count=2)");
    expect(s.tooltip).toBe("tray.tipApps(count=2)");
  });

  it("版本缺失时用占位符（不产出 undefined 文案）", () => {
    const s = buildTrayLabels(t, { hasCoreUpdate: true, appUpdateCount: 0 });
    expect(s.firstLabel).toBe("tray.firstUpdate(version=?)");
  });
});
