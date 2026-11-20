/**
 * 日志抽屉开关（跨组件）：外壳「查看日志」按钮 ↔ S4 页内抽屉。
 *
 * 回归防护：旧版底栏「查看日志」打开的是设置页（里面没有日志），
 * 用户在最需要自救时点进一个空页面。
 */
import { describe, expect, it } from "vitest";
import { createLogDrawerController } from "./logDrawer";

describe("logDrawer", () => {
  it("打开/关闭/切换 + 订阅通知", () => {
    const c = createLogDrawerController();
    const seen: boolean[] = [];
    const off = c.subscribe((v) => seen.push(v));
    expect(c.isOpen()).toBe(false);

    c.open();
    expect(c.isOpen()).toBe(true);
    c.close();
    expect(c.isOpen()).toBe(false);
    c.toggle();
    expect(c.isOpen()).toBe(true);

    // 重复 open 不重复通知
    const before = seen.length;
    c.open();
    expect(seen.length).toBe(before);

    off();
    const after = seen.length;
    c.close();
    expect(seen.length).toBe(after);
  });

  it("订阅时立即收到当前值", () => {
    const c = createLogDrawerController();
    c.open();
    const seen: boolean[] = [];
    c.subscribe((v) => seen.push(v));
    expect(seen).toEqual([true]);
  });
});
