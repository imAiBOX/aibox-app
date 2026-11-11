/**
 * 更新提醒纯逻辑：installed vs latest 比对 / 目录新增 diff（首启不提示）/ 4h 门控时间注入。
 */
import { describe, expect, it } from "vitest";
import {
  APP_UPDATE_TTL_MS,
  diffAppUpdates,
  diffNewApps,
  isCheckDue,
  isNewer,
} from "./appUpdates";

describe("isNewer（版本比较，与 Rust is_newer 同语义）", () => {
  it("数字段逐段比较，v 前缀可选", () => {
    expect(isNewer("v2026.11.2", "2026.11.1")).toBe(true);
    expect(isNewer("2026.11.2", "2026.11.2")).toBe(false);
    expect(isNewer("2026.11.1", "2026.11.2")).toBe(false);
    expect(isNewer("1.0.1", "1.0.0")).toBe(true);
    expect(isNewer("1.0.0", "1.0.1")).toBe(false);
  });
  it("非数字段字典序；空段补 0", () => {
    expect(isNewer("2.0.0", "1.9.9")).toBe(true);
    expect(isNewer("1.0", "1.0.0")).toBe(false);
  });
});

describe("diffAppUpdates（installed vs latest 比对）", () => {
  it("上游更新 → hasUpdate；同版/本地更新 → 不提醒", () => {
    const rows = diffAppUpdates(
      [
        { id: "a", version: "1.0.0" },
        { id: "b", version: "2.0.0" },
        { id: "c", version: "3.0.0" },
      ],
      { a: "1.1.0", b: "2.0.0", c: "2.9.0" },
    );
    expect(rows.map((r) => r.hasUpdate)).toEqual([true, false, false]);
    expect(rows[0]).toEqual({
      appId: "a",
      installed: "1.0.0",
      latest: "1.1.0",
      hasUpdate: true,
    });
  });

  it("缺 latest（registry 无记录）或缺 installed → 不提醒，字段省略", () => {
    const rows = diffAppUpdates(
      [
        { id: "ghost", version: "1.0.0" },
        { id: "nover" },
      ],
      { nover: "2.0.0" },
    );
    expect(rows[0].hasUpdate).toBe(false);
    expect(rows[0].latest).toBeUndefined();
    expect(rows[1].hasUpdate).toBe(false);
    expect(rows[1].installed).toBeUndefined();
    expect(rows[1].latest).toBe("2.0.0");
  });

  it("空已装列表 → 空行（未装任何应用零打扰）", () => {
    expect(diffAppUpdates([], { a: "1.0.0" })).toEqual([]);
  });
});

describe("diffNewApps（目录新增；首启不提示）", () => {
  it("首启（seen=null）→ []（避免首启轰炸）", () => {
    expect(diffNewApps(null, ["a", "b", "c"])).toEqual([]);
  });

  it("有记录 → 只报没见过的 id（保序）", () => {
    expect(diffNewApps(["a"], ["a", "c", "b", "c"])).toEqual(["c", "b"]);
  });

  it("无新增 → []", () => {
    expect(diffNewApps(["a", "b"], ["a", "b"])).toEqual([]);
  });

  it("下架的应用不会被当成新增", () => {
    expect(diffNewApps(["a", "b"], ["a"])).toEqual([]);
  });
});

describe("isCheckDue（4h 门控，时间注入）", () => {
  it("无记录 → 到期（启动即查）", () => {
    expect(isCheckDue(null, 1_000_000, APP_UPDATE_TTL_MS)).toBe(true);
    expect(isCheckDue(undefined, 1_000_000, APP_UPDATE_TTL_MS)).toBe(true);
  });

  it("距上次 < 4h → 不到期；≥ 4h → 到期", () => {
    const now = 10_000_000;
    expect(isCheckDue(now - APP_UPDATE_TTL_MS + 1, now, APP_UPDATE_TTL_MS)).toBe(false);
    expect(isCheckDue(now - APP_UPDATE_TTL_MS, now, APP_UPDATE_TTL_MS)).toBe(true);
    expect(isCheckDue(now - APP_UPDATE_TTL_MS * 2, now, APP_UPDATE_TTL_MS)).toBe(true);
  });

  it("ttl 可注入（任意节奏）", () => {
    expect(isCheckDue(1_000, 5_000, 3_000)).toBe(true);
    expect(isCheckDue(1_000, 5_000, 10_000)).toBe(false);
  });
});
