/**
 * 还原状态机单测（纯逻辑；不碰 IO）。
 * 覆盖：状态转移 / **默认不勾选**（硬规则①）/ **未勾不能跑**（硬规则②）/
 *      路径折叠 / 逐条原因码映射（9 个固定枚举 + 未知兜底）。
 */
import { describe, expect, it } from "vitest";
import {
  canConfirmRestore,
  createInitialRestoreState,
  formatBytes,
  restoreIssueKey,
  restoreReduce,
  visiblePlanEntries,
  type RestoreEvent,
  type RestoreState,
} from "./restore";
import type { BackupRecord, PlannedEntry, RestorePlan, RestorePreview } from "../net/client";

function entry(i: number): PlannedEntry {
  return {
    target: `C:\\Users\\dev\\.config\\openclaw\\file-${i}.json5`,
    rel: `.config/openclaw/file-${i}.json5`,
    size: 100,
    isDir: false,
    exists: false,
    payloadPresent: true,
    sha256: "0".repeat(64),
  };
}

function plan(n = 3): RestorePlan {
  return {
    appId: "openclaw",
    backupDir: "C:\\Users\\dev\\.aibox\\backup\\20261122120000\\openclaw",
    createdAt: "2026-11-22T12:00:00.000Z",
    createdAtMs: 1792737600000,
    entries: Array.from({ length: n }, (_, i) => entry(i)),
    fileCount: n,
    dirCount: 0,
    totalBytes: n * 100,
    wouldPreserve: 0,
    digest: "digest-abc",
    maxAgeHours: 24,
  };
}

function preview(n = 3): RestorePreview {
  return {
    appId: "openclaw",
    plan: plan(n),
    registrySource: "github-api",
    dataPaths: ["~/.config/openclaw"],
    maxAgeHours: 24,
  };
}

function backup(id = "openclaw"): BackupRecord {
  return {
    appId: id,
    dir: "C:\\Users\\dev\\.aibox\\backup\\20261122120000\\openclaw",
    createdAt: "2026-11-22T12:00:00.000Z",
    createdAtMs: 1792737600000,
    fileCount: 3,
    totalBytes: 300,
    format: "v1",
    restorable: true,
    ageHours: 1,
  };
}

function run(events: RestoreEvent[], from: RestoreState = createInitialRestoreState()): RestoreState {
  return events.reduce(restoreReduce, from);
}

describe("还原状态机", () => {
  it("初始态：idle / 不勾选 / 无数据", () => {
    const s = createInitialRestoreState();
    expect(s.step).toBe("idle");
    expect(s.ack).toBe(false);
    expect(s.preview).toBeNull();
    expect(s.result).toBeNull();
    expect(s.error).toBeNull();
  });

  it("列表：loading → loaded → error → loaded", () => {
    let s = run([{ type: "R_LIST_LOADING" }]);
    expect(s.step).toBe("listing");
    s = restoreReduce(s, { type: "R_LIST_LOADED", backups: [backup()] });
    expect(s.step).toBe("idle");
    expect(s.backups).toHaveLength(1);
    s = restoreReduce(s, { type: "R_LIST_ERROR" });
    expect(s.step).toBe("listError");
    s = restoreReduce(s, { type: "R_LIST_LOADED", backups: [] });
    expect(s.step).toBe("idle");
    expect(s.backups).toHaveLength(0);
  });

  it("预览：previewing → preview（新预览一律不勾选）", () => {
    let s = run([{ type: "R_PREVIEW_LOADING" }]);
    expect(s.step).toBe("previewing");
    s = restoreReduce(s, { type: "R_PREVIEW_LOADED", preview: preview() });
    expect(s.step).toBe("preview");
    expect(s.preview?.plan.digest).toBe("digest-abc");
    expect(s.ack).toBe(false);
  });

  it("重新预览 → 勾选被清掉（不能拿旧确认去跑新备份）", () => {
    let s = run([
      { type: "R_PREVIEW_LOADED", preview: preview() },
      { type: "R_ACK", ack: true },
    ]);
    expect(s.ack).toBe(true);
    s = restoreReduce(s, { type: "R_PREVIEW_LOADING" });
    expect(s.ack).toBe(false);
    expect(s.preview).toBeNull();
  });

  it("硬规则②：未勾选 → R_RUN 被忽略（不执行）", () => {
    const s = run([
      { type: "R_PREVIEW_LOADED", preview: preview() },
      { type: "R_RUN" },
    ]);
    expect(s.step).toBe("preview");
    expect(canConfirmRestore(s)).toBe(false);
  });

  it("硬规则②：勾选后 → R_RUN 进入 running；done → done", () => {
    let s = run([
      { type: "R_PREVIEW_LOADED", preview: preview() },
      { type: "R_ACK", ack: true },
    ]);
    expect(canConfirmRestore(s)).toBe(true);
    s = restoreReduce(s, { type: "R_RUN" });
    expect(s.step).toBe("running");
    s = restoreReduce(s, { type: "R_DONE", result: { restored: 2 } });
    expect(s.step).toBe("done");
    expect(s.result?.restored).toBe(2);
  });

  it("非预览态不接受勾选 / 展开（幂等，不崩）", () => {
    const s = run([
      { type: "R_ACK", ack: true },
      { type: "R_EXPAND", expanded: true },
    ]);
    expect(s.ack).toBe(false);
    expect(s.expanded).toBe(false);
  });

  it("错误：preview_error / error → step=error；close → 复位", () => {
    let s = run([
      { type: "R_PREVIEW_ERROR", code: "RESTORE_UNSUPPORTED", messageKey: "s8.errRestoreUnsupported" },
    ]);
    expect(s.step).toBe("error");
    expect(s.error?.messageKey).toBe("s8.errRestoreUnsupported");
    s = restoreReduce(s, { type: "R_CLOSE" });
    expect(s.step).toBe("idle");
    expect(s.error).toBeNull();
  });

  it("展开/收起：R_EXPAND 只在预览态生效", () => {
    let s = run([{ type: "R_PREVIEW_LOADED", preview: preview(30) }]);
    s = restoreReduce(s, { type: "R_EXPAND", expanded: true });
    expect(s.expanded).toBe(true);
  });
});

describe("visiblePlanEntries（路径折叠）", () => {
  it("≤20 条 → 全显示，hidden=0", () => {
    const { shown, hidden } = visiblePlanEntries(plan(3));
    expect(shown).toHaveLength(3);
    expect(hidden).toBe(0);
  });

  it(">20 条 → 默认只显示 20 条，其余计数", () => {
    const { shown, hidden } = visiblePlanEntries(plan(30));
    expect(shown).toHaveLength(20);
    expect(hidden).toBe(10);
  });

  it("limit 可调；空计划不炸", () => {
    expect(visiblePlanEntries(plan(30), 5).shown).toHaveLength(5);
    expect(visiblePlanEntries(null).shown).toHaveLength(0);
    expect(visiblePlanEntries(null).hidden).toBe(0);
  });
});

describe("restoreIssueKey（逐条原因码 → 人话 key）", () => {
  it("9 个固定枚举各有一个 key", () => {
    expect(restoreIssueKey("symlink")).toBe("s8.restoreReasonSymlink");
    expect(restoreIssueKey("sha_mismatch")).toBe("s8.restoreReasonSha");
    expect(restoreIssueKey("type_conflict")).toBe("s8.restoreReasonType");
    expect(restoreIssueKey("outside_backup")).toBe("s8.restoreReasonOutside");
    expect(restoreIssueKey("source_missing")).toBe("s8.restoreReasonMissing");
    expect(restoreIssueKey("preserve_failed")).toBe("s8.restoreReasonPreserve");
    expect(restoreIssueKey("write_failed")).toBe("s8.restoreReasonWrite");
    expect(restoreIssueKey("verify_failed")).toBe("s8.restoreReasonVerify");
    expect(restoreIssueKey("parent_conflict")).toBe("s8.restoreReasonParent");
  });

  it("未知码 → 通用兜底（不把机器码甩给用户）", () => {
    expect(restoreIssueKey("brand_new_code")).toBe("s8.restoreReasonUnknown");
    expect(restoreIssueKey("")).toBe("s8.restoreReasonUnknown");
  });
});

describe("formatBytes", () => {
  it("B / KB / MB / GB", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KB");
    expect(formatBytes(5 * 1024 * 1024)).toBe("5.0 MB");
    expect(formatBytes(3 * 1024 * 1024 * 1024)).toBe("3.00 GB");
  });

  it("负数/NaN → 0 B（不显示负数）", () => {
    expect(formatBytes(-1)).toBe("0 B");
    expect(formatBytes(Number.NaN)).toBe("0 B");
  });
});
