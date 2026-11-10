/**
 * 状态机转移表全分支测试。
 * 每条转移 + 每条非法转移（不改变状态）+ ERROR 恢复 + 断点续装。
 */
import { describe, expect, it } from "vitest";
import {
  createInitialState,
  type Event,
  type MachineState,
  type Plan,
  type ScanReport,
  type Status,
} from "./types";
import { reduce as reducer } from "./reducer";

const report: ScanReport = {
  os: { platform: "windows", version: "Windows 11", arch: "x64" },
  diskFreeGb: 100,
  memTotalGb: 16,
  node: { installed: false },
  git: { installed: true, version: "2.56.0" },
  wsl: { installed: false },
  installedApps: [],
  port18789: { occupied: false },
  checks: [{ id: "os", level: "ok", human: "scan.os" }],
};

const plan: Plan = {
  appId: "openclaw",
  nodes: [
    { id: "node", name: "Node.js", versionRange: "v26", kind: "install", children: ["oc"] },
    { id: "oc", name: "OpenClaw", versionRange: "2026.11.2", kind: "target", children: [] },
  ],
  tasks: [
    { id: "node", name: "Node.js", version: "v26", phase: "download", status: "pending" },
    { id: "oc", name: "OpenClaw", version: "2026.11.2", phase: "download", status: "pending" },
  ],
  route: "ghfast.top",
  channel: "ghfast.top",
  diskMb: 700,
  estMinutes: 6,
};

function at(status: Status): MachineState {
  const s = createInitialState();
  return { ...s, status };
}

/** 沿主流程推进到某态 */
function flowTo(status: Status): MachineState {
  let s = createInitialState();
  s = reducer(s, { type: "START_INSTALL" });
  s = reducer(s, { type: "SCAN_DONE", report });
  if (status === "SCAN") return s;
  s = reducer(s, { type: "PLAN_READY", plan });
  if (status === "PLAN") return s;
  s = reducer(s, { type: "CONFIRM" });
  if (status === "CONFIRM") return s;
  s = reducer(s, { type: "BEGIN" });
  if (status === "EXECUTE") return s;
  s = reducer(s, { type: "TASK_PHASE", taskId: "node", phase: "download" });
  s = reducer(s, { type: "TASK_DONE", taskId: "node" });
  s = reducer(s, { type: "TASK_PHASE", taskId: "oc", phase: "download" });
  s = reducer(s, { type: "TASK_DONE", taskId: "oc" });
  s = reducer(s, { type: "ALL_DONE" });
  if (status === "CONFIG") return s;
  s = reducer(s, { type: "CONFIG_WRITTEN", pathId: "openrouter" });
  if (status === "VERIFY") return s;
  s = reducer(s, { type: "VERIFY_OK" });
  return s;
}

describe("主流程转移表（IDLE→…→DONE）", () => {
  it("IDLE →(START_INSTALL)→ SCAN", () => {
    const s = reducer(createInitialState(), { type: "START_INSTALL" });
    expect(s.status).toBe("SCAN");
    expect(s.error).toBeNull();
  });

  it("SCAN →(SCAN_DONE)→ PLAN（携带报告）", () => {
    const s = reducer(at("SCAN"), { type: "SCAN_DONE", report });
    expect(s.status).toBe("PLAN");
    expect(s.scan).toEqual(report);
  });

  it("PLAN →(PLAN_READY) 自转移（注入计划与任务队列）", () => {
    const s = reducer(at("PLAN"), { type: "PLAN_READY", plan });
    expect(s.status).toBe("PLAN");
    expect(s.plan).toEqual(plan);
    expect(s.tasks).toHaveLength(2);
    expect(s.currentTaskIndex).toBe(0);
  });

  it("PLAN →(CONFIRM)→ CONFIRM", () => {
    expect(reducer(at("PLAN"), { type: "CONFIRM" }).status).toBe("CONFIRM");
  });

  it("CONFIRM →(BEGIN)→ EXECUTE（任务队列就位）", () => {
    const s = reducer({ ...flowTo("CONFIRM"), tasks: plan.tasks }, { type: "BEGIN" });
    expect(s.status).toBe("EXECUTE");
    expect(s.currentTaskIndex).toBe(0);
  });

  it("EXECUTE TASK_PHASE 推进阶段并标记 running", () => {
    const base = { ...flowTo("EXECUTE") };
    const s = reducer(base, { type: "TASK_PHASE", taskId: "node", phase: "verify" });
    expect(s.status).toBe("EXECUTE");
    expect(s.tasks[0]).toMatchObject({ phase: "verify", status: "running" });
    expect(s.currentTaskIndex).toBe(0);
  });

  it("TASK_PHASE 指向未知任务 → 保持 index", () => {
    const s = reducer(flowTo("EXECUTE"), { type: "TASK_PHASE", taskId: "nope", phase: "verify" });
    expect(s.currentTaskIndex).toBe(0);
  });

  it("TASK_DONE 标记完成；全部完成后 ALL_DONE → CONFIG", () => {
    let s = flowTo("EXECUTE");
    s = reducer(s, { type: "TASK_DONE", taskId: "node" });
    s = reducer(s, { type: "TASK_DONE", taskId: "oc" });
    expect(s.tasks.every((t) => t.status === "done")).toBe(true);
    const c = reducer(s, { type: "ALL_DONE" });
    expect(c.status).toBe("CONFIG");
  });

  it("ALL_DONE 在任务未完成时非法（不转移）", () => {
    const s = reducer(flowTo("EXECUTE"), { type: "ALL_DONE" });
    expect(s.status).toBe("EXECUTE");
  });

  it("CONFIG →(CONFIG_WRITTEN)→ VERIFY（记录 pathId）", () => {
    const s = reducer(at("CONFIG"), { type: "CONFIG_WRITTEN", pathId: "openai" });
    expect(s.status).toBe("VERIFY");
    expect(s.configPathId).toBe("openai");
  });

  it("VERIFY →(VERIFY_OK)→ DONE", () => {
    const s = reducer(at("VERIFY"), { type: "VERIFY_OK" });
    expect(s.status).toBe("DONE");
    expect(s.verifyOk).toBe(true);
  });

  it("DONE →(RESTART)→ IDLE（保留体检结果）", () => {
    const s = reducer({ ...flowTo("DONE"), scan: report }, { type: "RESTART" });
    expect(s.status).toBe("IDLE");
    expect(s.scan).toEqual(report);
    expect(s.plan).toBeNull();
  });
});

describe("CONFIG_SELECT / BACK", () => {
  it("CONFIG_SELECT 仅在 CONFIG 生效", () => {
    const s = reducer(at("CONFIG"), { type: "CONFIG_SELECT", pathId: "anthropic" });
    expect(s.status).toBe("CONFIG");
    expect(s.configPathId).toBe("anthropic");
    const bad = reducer(at("VERIFY"), { type: "CONFIG_SELECT", pathId: "anthropic" });
    expect(bad.configPathId).toBeNull();
  });

  it("BACK：SCAN→IDLE、PLAN→SCAN、CONFIRM→PLAN", () => {
    expect(reducer(at("SCAN"), { type: "BACK" }).status).toBe("IDLE");
    expect(reducer(at("PLAN"), { type: "BACK" }).status).toBe("SCAN");
    expect(reducer(at("CONFIRM"), { type: "BACK" }).status).toBe("PLAN");
  });

  it("BACK 在 EXECUTE/DONE 非法", () => {
    expect(reducer(at("EXECUTE"), { type: "BACK" }).status).toBe("EXECUTE");
    expect(reducer(at("DONE"), { type: "BACK" }).status).toBe("DONE");
  });
});

describe("任意态 → ERROR → 恢复", () => {
  const ALL: Status[] = [
    "IDLE",
    "SCAN",
    "PLAN",
    "CONFIRM",
    "EXECUTE",
    "CONFIG",
    "VERIFY",
    "DONE",
  ];

  it("全部 8 个主流程态都能进 ERROR，并记录 from", () => {
    for (const st of ALL) {
      const s = reducer(at(st), {
        type: "ERROR",
        code: "NET_ALL_CHANNELS_DEAD",
        messageKey: "err.netDead",
      });
      expect(s.status).toBe(st === "DONE" ? "ERROR" : "ERROR");
      expect(s.error?.from).toBe(st);
      expect(s.error?.code).toBe("NET_ALL_CHANNELS_DEAD");
    }
  });

  it("ERROR →(RETRY)→ 出错原态", () => {
    for (const st of ALL) {
      const err = reducer(at(st), { type: "ERROR", code: "X", messageKey: "err.x" });
      const back = reducer(err, { type: "RETRY" });
      expect(back.status).toBe(st);
      expect(back.error).toBeNull();
    }
  });

  it("ERROR →(EXPORT_DIAG)→ 出错原态（导出为副作用）", () => {
    const err = reducer(at("EXECUTE"), { type: "ERROR", code: "X", messageKey: "err.x" });
    expect(reducer(err, { type: "EXPORT_DIAG" }).status).toBe("EXECUTE");
  });

  it("ERROR →(SKIP)→ 出错原态 + 跳过失败任务并推进边界", () => {
    let s = flowTo("EXECUTE");
    s = reducer(s, { type: "ERROR", code: "HASH_MISMATCH", messageKey: "err.hash", taskId: "node" });
    expect(s.error?.taskId).toBe("node");
    // 标记失败任务
    s = { ...s, tasks: s.tasks.map((t, i) => (i === 0 ? { ...t, status: "failed" as const } : t)) };
    const after = reducer(s, { type: "SKIP" });
    expect(after.status).toBe("EXECUTE");
    expect(after.tasks[0].status).toBe("skipped");
    expect(after.currentTaskIndex).toBe(1);
    expect(after.error).toBeNull();
  });

  it("RETRY/SKIP/EXPORT_DIAG 在非 ERROR 态非法", () => {
    for (const ev of ["RETRY", "SKIP", "EXPORT_DIAG"] as const) {
      expect(reducer(at("EXECUTE"), { type: ev }).status).toBe("EXECUTE");
    }
  });

  it("ERROR 态可再次 ERROR（覆盖 from 为当前 ERROR）", () => {
    const e1 = reducer(at("EXECUTE"), { type: "ERROR", code: "A", messageKey: "err.a" });
    const e2 = reducer(e1, { type: "ERROR", code: "B", messageKey: "err.b" });
    expect(e2.error?.from).toBe("ERROR");
  });
});

describe("断点续装（IDLE 检测未完成会话）", () => {
  const snap = { appId: "openclaw", doneTaskIds: ["node"], savedAt: 1 };

  it("IDLE →(DETECT_SESSION) 弹继续询问", () => {
    const s = reducer(createInitialState(), { type: "DETECT_SESSION", snapshot: snap });
    expect(s.status).toBe("IDLE");
    expect(s.resume).toEqual({ available: true, snapshot: snap, prompt: true });
  });

  it("DETECT_SESSION 在非 IDLE 态被忽略", () => {
    const s = reducer(at("EXECUTE"), { type: "DETECT_SESSION", snapshot: snap });
    expect(s.resume.available).toBe(false);
  });

  it("RESUME_SESSION → EXECUTE 该任务边界（已完成任务保持 done）", () => {
    const withSnap = reducer(createInitialState(), { type: "DETECT_SESSION", snapshot: snap });
    const resumed = reducer({ ...withSnap, plan, tasks: plan.tasks }, { type: "RESUME_SESSION" });
    expect(resumed.status).toBe("EXECUTE");
    expect(resumed.tasks[0].status).toBe("done");
    expect(resumed.tasks[1].status).toBe("pending");
    expect(resumed.currentTaskIndex).toBe(1);
    expect(resumed.resume.prompt).toBe(false);
  });

  it("RESUME_SESSION 无快照时非法", () => {
    const s = reducer(createInitialState(), { type: "RESUME_SESSION" });
    expect(s.status).toBe("IDLE");
  });

  it("DISMISS_RESUME 关闭弹层但保留 available", () => {
    const withSnap = reducer(createInitialState(), { type: "DETECT_SESSION", snapshot: snap });
    const s = reducer(withSnap, { type: "DISMISS_RESUME" });
    expect(s.resume.prompt).toBe(false);
    expect(s.resume.available).toBe(true);
    expect(reducer(s, { type: "DISMISS_RESUME" }).resume.prompt).toBe(false);
  });
});

describe("非法事件（当前态不接受）→ 状态不变", () => {
  const cases: { from: Status; ev: Event }[] = [
    { from: "SCAN", ev: { type: "START_INSTALL" } },
    { from: "PLAN", ev: { type: "SCAN_DONE", report } },
    { from: "CONFIRM", ev: { type: "CONFIRM" } },
    { from: "IDLE", ev: { type: "BEGIN" } },
    { from: "IDLE", ev: { type: "ALL_DONE" } },
    { from: "SCAN", ev: { type: "CONFIG_WRITTEN", pathId: "x" } },
    { from: "CONFIG", ev: { type: "VERIFY_OK" } },
    { from: "VERIFY", ev: { type: "RESTART" } },
    { from: "EXECUTE", ev: { type: "TASK_DONE", taskId: "node" } }, // 仍生效但这里验证状态不变
    { from: "IDLE", ev: { type: "PLAN_READY", plan } },
    { from: "IDLE", ev: { type: "TASK_PHASE", taskId: "node", phase: "verify" } },
  ];

  for (const { from, ev } of cases) {
    it(`${from} rejects ${ev.type} (status unchanged)`, () => {
      const before = at(from);
      const after = reducer(before, ev);
      // TASK_DONE/PLAN_READY 等在错误态注入数据不改变 status
      expect(after.status).toBe(from);
    });
  }
});

describe("flowTo 编排辅助自检", () => {
  it("走到 DONE 全程状态正确", () => {
    expect(flowTo("DONE").status).toBe("DONE");
    expect(flowTo("CONFIG").status).toBe("CONFIG");
    expect(flowTo("VERIFY").status).toBe("VERIFY");
  });
});
