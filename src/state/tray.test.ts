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

describe("托盘待办（状态面板）", () => {
  const t = (k: string, o?: Record<string, string | number>) => `${k}${o ? JSON.stringify(o) : ""}`;

  it("角标数字 = 待办总数；无待办 → 空（不显示角标）", () => {
    const none = buildTrayLabels(t, { hasCoreUpdate: false, appUpdateCount: 0 });
    expect(none.badge).toBe("");
    expect(none.pendingUpdatesCount).toBe(0);
    expect(none.pendingNewCount).toBe(0);
    expect(none.pendingCoreCount).toBe(0);

    const some = buildTrayLabels(t, {
      hasCoreUpdate: true,
      coreVersion: "0.1.11",
      appUpdateCount: 2,
      newAppCount: 1,
    });
    expect(some.badge).toBe("4");
    expect(some.pendingUpdatesCount).toBe(2);
    expect(some.pendingNewCount).toBe(1);
    expect(some.pendingCoreCount).toBe(1);
  });

  it("只有目录上新时，首项与 tooltip 指向新应用", () => {
    const r = buildTrayLabels(t, { hasCoreUpdate: false, appUpdateCount: 0, newAppCount: 3 });
    expect(r.firstAction).toBe("open");
    expect(r.firstLabel).toContain("tray.newApps");
    expect(r.tooltip).toContain("tray.tipNew");
  });
});
