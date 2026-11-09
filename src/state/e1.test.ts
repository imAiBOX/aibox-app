/**
 * E1 内嵌诊断面板触发器：同一任务连败 2 次触发；换任务/成功重置；ACK 收起防连弹。
 */
import { describe, expect, it } from "vitest";
import { createE1State, e1Reduce, E1_THRESHOLD } from "./e1";

describe("e1Reduce", () => {
  it("阈值为 2", () => {
    expect(E1_THRESHOLD).toBe(2);
  });

  it("同一任务第 1 次失败不触发，第 2 次触发面板", () => {
    let s = createE1State();
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    expect(s.open).toBe(false);
    expect(s.count).toBe(1);
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    expect(s.open).toBe(true);
    expect(s.count).toBe(2);
    expect(s.taskId).toBe("node");
  });

  it("换任务 → 计数重置（不累计到 2）", () => {
    let s = createE1State();
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    s = e1Reduce(s, { type: "FAIL", taskId: "git" });
    expect(s.count).toBe(1);
    expect(s.open).toBe(false);
    s = e1Reduce(s, { type: "FAIL", taskId: "git" });
    expect(s.open).toBe(true);
  });

  it("ACK → 收起且防连弹（armed 清除后重新计数）", () => {
    let s = createE1State();
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    expect(s.open).toBe(true);
    s = e1Reduce(s, { type: "ACK" });
    expect(s.open).toBe(false);
    expect(s.count).toBe(0);
    // ACK 后再失败 1 次不立即弹（重新从 1 计数）
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    expect(s.open).toBe(false);
  });

  it("任务成功 → 连败链断裂全重置", () => {
    let s = createE1State();
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    s = e1Reduce(s, { type: "SUCCESS", taskId: "node" });
    expect(s).toEqual(createE1State());
  });

  it("SUCCESS 不清其他任务的计数（taskId 不匹配保持原状）", () => {
    let s = createE1State();
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    s = e1Reduce(s, { type: "SUCCESS", taskId: "other" });
    expect(s.count).toBe(1);
    expect(s.taskId).toBe("node");
  });

  it("无 taskId 的失败（如扫描失败）单独计数，不串到任务上", () => {
    let s = createE1State();
    s = e1Reduce(s, { type: "FAIL" });
    expect(s.taskId).toBeNull();
    expect(s.count).toBe(1);
    s = e1Reduce(s, { type: "FAIL", taskId: "node" });
    expect(s.count).toBe(1); // 换了身份 → 重计
  });

  it("RESET 回初始态", () => {
    let s = createE1State();
    s = e1Reduce(s, { type: "FAIL", taskId: "a" });
    s = e1Reduce(s, { type: "FAIL", taskId: "a" });
    s = e1Reduce(s, { type: "RESET" });
    expect(s).toEqual(createE1State());
  });
});
