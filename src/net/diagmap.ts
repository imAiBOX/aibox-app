/**
 * DiagReport → 体检卡渲染行（E2 设置-网络 / E1 内嵌面板共用）。
 * 纯函数：summary 按当前语言取值（缺省回落 en），status→三色方块，fixes→R1/R2/R3 徽章。
 */
import type { DiagReport, DiagReportItem } from "./client";

export type DiagDot = "ok" | "warn" | "fail";

export interface DiagRow {
  key: string;
  item: string;
  dot: DiagDot;
  /** 当前语言人话结论（缺省回落 en / item 编号） */
  summary: string;
  fixes: string[];
  detail?: string;
  timestampMs: number;
}

export const DOT_CLASS: Record<DiagDot, string> = {
  ok: "bg-aibox-ok",
  warn: "bg-[#f5c518]",
  fail: "bg-aibox-danger",
};

export function mapDiagRows(report: DiagReport | null, lang: string): DiagRow[] {
  if (!report) return [];
  const zh = lang.toLowerCase().startsWith("zh");
  return report.items.map((it: DiagReportItem) => ({
    key: it.item,
    item: it.item,
    dot: (it.status === "ok" || it.status === "warn" || it.status === "fail"
      ? it.status
      : "warn") as DiagDot,
    summary: (zh ? it.summary?.["zh-CN"] : it.summary?.en) || it.summary?.en || it.item,
    fixes: it.fixes ?? [],
    detail: it.detail,
    timestampMs: it.timestampMs ?? 0,
  }));
}

/** 报告里最严重的状态（面板头部一眼看；无报告→null） */
export function worstStatus(rows: DiagRow[]): DiagDot | null {
  if (rows.some((r) => r.dot === "fail")) return "fail";
  if (rows.some((r) => r.dot === "warn")) return "warn";
  if (rows.length > 0) return "ok";
  return null;
}
