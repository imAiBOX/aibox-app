/**
 * 关键 selector 测试。
 */
import { describe, expect, it } from "vitest";
import {
  createInitialState,
  errorActions,
  hasResumableSession,
  inFlow,
  navIndex,
  progress,
  screenFor,
  type MachineState,
  type Plan,
} from "./index";

const plan: Plan = {
  appId: "openclaw",
  nodes: [],
  tasks: [
    { id: "a", name: "Node.js", version: "v26", phase: "download", status: "done" },
    { id: "b", name: "Git", version: "2.56", phase: "download", status: "running" },
    { id: "c", name: "OpenClaw", version: "2026.11.2", phase: "download", status: "pending" },
  ],
  route: "ghfast.top",
  channel: "ghfast.top",
  diskMb: 700,
  estMinutes: 6,
};

function execState(): MachineState {
  return { ...createInitialState(), status: "EXECUTE", plan, tasks: plan.tasks, currentTaskIndex: 1 };
}

describe("screenFor 屏幕映射", () => {
  it("八态 + ERROR 映射", () => {
    expect(screenFor("IDLE")).toBe("S1Idle");
    expect(screenFor("SCAN")).toBe("S2Scan");
    expect(screenFor("PLAN")).toBe("S3Plan");
    expect(screenFor("CONFIRM")).toBe("S3Plan");
    expect(screenFor("EXECUTE")).toBe("S4Execute");
    expect(screenFor("CONFIG")).toBe("S5Config");
    expect(screenFor("VERIFY")).toBe("S6Verify");
    expect(screenFor("DONE")).toBe("S7Done");
    expect(screenFor("ERROR")).toBe("S4Execute"); // 覆盖层，底层保持
  });
});

describe("navIndex 左侧六态导航", () => {
  it("主流程各态高亮格", () => {
    expect(navIndex("IDLE")).toBe(0);
    expect(navIndex("SCAN")).toBe(1);
    expect(navIndex("PLAN")).toBe(2);
    expect(navIndex("CONFIRM")).toBe(2); // CONFIRM 属 PLAN 步
    expect(navIndex("EXECUTE")).toBe(3);
    expect(navIndex("CONFIG")).toBe(4);
    expect(navIndex("VERIFY")).toBe(5);
    expect(navIndex("DONE")).toBe(5); // 终态收尾
  });
  it("ERROR 高亮出错前的态", () => {
    expect(navIndex("ERROR", "CONFIG")).toBe(4);
    expect(navIndex("ERROR")).toBe(3); // 默认 EXECUTE
  });
});

describe("progress 进度（4 阶段/任务）", () => {
  it("混合状态计算", () => {
    const p = progress(execState());
    // a=done 4 + b=running download(0) + c pending → 4/12
    expect(p.total).toBe(12);
    expect(p.done).toBe(4);
    expect(p.pct).toBe(33);
  });
  it("空任务队列 → 0", () => {
    expect(progress(createInitialState())).toEqual({ done: 0, total: 0, pct: 0 });
  });
  it("running 在 install 阶段 → 4+2", () => {
    const s = execState();
    s.tasks = s.tasks.map((t, i) => (i === 1 ? { ...t, phase: "install" as const } : t));
    expect(progress(s).done).toBe(6);
  });
  it("skipped 计满 4", () => {
    const s = execState();
    s.tasks = s.tasks.map((t, i) => (i === 1 ? { ...t, status: "skipped" as const } : t));
    expect(progress(s).done).toBe(8);
  });
});

describe("errorActions 三按钮可用性", () => {
  it("非 ERROR → 全 false", () => {
    expect(errorActions(createInitialState())).toEqual({
      retry: false,
      skip: false,
      exportDiag: false,
    });
  });
  it("ERROR@EXECUTE 且有任务 → retry/skip/export 全 true", () => {
    const s: MachineState = {
      ...execState(),
      status: "ERROR",
      error: { code: "X", messageKey: "err.x", from: "EXECUTE" },
    };
    expect(errorActions(s)).toEqual({ retry: true, skip: true, exportDiag: true });
  });
  it("ERROR@SCAN → skip false（无可跳任务）", () => {
    const s: MachineState = {
      ...createInitialState(),
      status: "ERROR",
      error: { code: "X", messageKey: "err.x", from: "SCAN" },
    };
    expect(errorActions(s)).toEqual({ retry: true, skip: false, exportDiag: true });
  });
});

describe("inFlow / hasResumableSession", () => {
  it("主流程 8 态 + ERROR 均算流程内", () => {
    for (const st of ["IDLE", "SCAN", "PLAN", "CONFIRM", "EXECUTE", "CONFIG", "VERIFY", "DONE", "ERROR"] as const) {
      expect(inFlow(st)).toBe(true);
    }
  });
  it("有快照且弹层展示 → true", () => {
    const s: MachineState = {
      ...createInitialState(),
      resume: {
        available: true,
        prompt: true,
        snapshot: { appId: "openclaw", doneTaskIds: ["a"], savedAt: 1 },
      },
    };
    expect(hasResumableSession(s)).toBe(true);
    expect(hasResumableSession(createInitialState())).toBe(false);
  });
});
