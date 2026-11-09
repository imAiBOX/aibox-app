/**
 * netdoctor 渲染映射（E2/E1 共用）：summary 按语言取值、三色方点、R 徽章、detail。
 */
import { describe, expect, it } from "vitest";
import { DOT_CLASS, mapDiagRows, worstStatus } from "./diagmap";
import type { DiagReport } from "./client";

const report: DiagReport = {
  schema: "aibox.diag.v1",
  generatedAtMs: 123,
  items: [
    {
      item: "D0",
      status: "ok",
      summary: { en: "DNS ok", "zh-CN": "DNS 正常" },
      timestampMs: 1,
    },
    {
      item: "D2",
      status: "warn",
      summary: { en: "Proxy found", "zh-CN": "检测到代理" },
      detail: "http=127.0.0.1:7890",
      timestampMs: 2,
      fixes: ["R1"],
    },
    {
      item: "D4",
      status: "fail",
      summary: { en: "No source", "zh-CN": "下载源不可达" },
      timestampMs: 3,
      fixes: ["R1", "R2"],
    },
  ],
};

describe("mapDiagRows", () => {
  it("中文语言 → zh-CN summary", () => {
    const rows = mapDiagRows(report, "zh-CN");
    expect(rows[0].summary).toBe("DNS 正常");
    expect(rows[1].summary).toBe("检测到代理");
  });

  it("英文语言 → en summary", () => {
    const rows = mapDiagRows(report, "en");
    expect(rows[0].summary).toBe("DNS ok");
  });

  it("zh-CN 缺失 → 回落 en", () => {
    const partial: DiagReport = {
      items: [
        { item: "D0", status: "ok", summary: { en: "DNS ok", "zh-CN": "" }, timestampMs: 1 },
      ],
    };
    expect(mapDiagRows(partial, "zh-CN")[0].summary).toBe("DNS ok");
  });

  it("状态映射 + 三色 class", () => {
    const rows = mapDiagRows(report, "en");
    expect(rows.map((r) => r.dot)).toEqual(["ok", "warn", "fail"]);
    expect(DOT_CLASS.ok).toContain("ok");
    expect(DOT_CLASS.fail).toContain("danger");
    expect(DOT_CLASS.warn).toBeTruthy();
  });

  it("fixes R1/R2/R3 徽章 + detail 透传", () => {
    const rows = mapDiagRows(report, "en");
    expect(rows[1].fixes).toEqual(["R1"]);
    expect(rows[2].fixes).toEqual(["R1", "R2"]);
    expect(rows[1].detail).toBe("http=127.0.0.1:7890");
  });

  it("null 报告 → 空数组", () => {
    expect(mapDiagRows(null, "en")).toEqual([]);
  });

  it("未知 status → warn（不炸）", () => {
    const weird: DiagReport = {
      items: [
        { item: "D0", status: "weird" as "ok", summary: { en: "x", "zh-CN": "x" }, timestampMs: 0 },
      ],
    };
    expect(mapDiagRows(weird, "en")[0].dot).toBe("warn");
  });
});

describe("worstStatus", () => {
  it("fail > warn > ok", () => {
    expect(worstStatus(mapDiagRows(report, "en"))).toBe("fail");
    const warnOnly = mapDiagRows(
      { items: [report.items[0], report.items[1]] },
      "en",
    );
    expect(worstStatus(warnOnly)).toBe("warn");
    expect(worstStatus(mapDiagRows({ items: [report.items[0]] }, "en"))).toBe("ok");
    expect(worstStatus([])).toBeNull();
  });
});
