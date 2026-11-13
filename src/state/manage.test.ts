/**
 * manageReducer 全分支 + 风险卡守卫 + 路径清单组装 单测。
 * 硬规则：备份默认勾选 / 不勾需二次确认 / 取消不触发 / 未确认不执行 / 清单只来自 registry。
 */
import { describe, expect, it } from "vitest";
import {
  buildRemovePaths,
  canConfirmRisk,
  createInitialManageState,
  manageReduce,
  type ManageEvent,
  type ManageState,
  type MaintenanceInfo,
} from "./manage";
import type { Task } from "./types";

function task(id: string, status: Task["status"] = "pending"): Task {
  return { id, name: id, version: "1.0", phase: "download", status };
}

function run(events: ManageEvent[], from: ManageState = createInitialManageState()): ManageState {
  return events.reduce(manageReduce, from);
}

describe("风险卡守卫（硬规则①②③）", () => {
  it("backupFirst=true → 默认勾选备份", () => {
    const s = run([
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: true },
    ]);
    expect(s.risk?.backup).toBe(true);
    expect(s.risk?.noBackupAck).toBe(false);
    expect(s.status).toBe("MANAGE");
  });

  it("registry backupFirst=false → 不默认勾选", () => {
    const s = run([
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: false },
    ]);
    expect(s.risk?.backup).toBe(false);
  });

  it("未确认不执行：不勾备份且无二次确认 → 确认被拒（原态）", () => {
    const s = run([
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: false },
      { type: "M_RISK_CONFIRM" },
    ]);
    expect(s.status).toBe("MANAGE");
    expect(s.op).toBeNull();
    expect(canConfirmRisk(s.risk!)).toBe(false);
  });

  it("不勾备份 + 二次确认 → 可执行；backup 定格 false", () => {
    const s = run([
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: false },
      { type: "M_RISK_NO_BACKUP_ACK", acked: true },
      { type: "M_RISK_CONFIRM" },
    ]);
    expect(s.status).toBe("REMOVE");
    expect(s.backup).toBe(false);
    expect(s.risk).toBeNull();
  });

  it("勾选备份 → 直接可执行（无需二次确认）", () => {
    const s = run([
      { type: "M_RISK_OPEN", op: "REINSTALL", appId: "ollama", backupFirst: true },
      { type: "M_RISK_CONFIRM" },
    ]);
    expect(s.status).toBe("REINSTALL");
    expect(s.backup).toBe(true);
  });

  it("取消勾选 → 旧的二次确认作废（必须重新确认）", () => {
    const s = run([
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: true },
      { type: "M_RISK_BACKUP", checked: false },
      { type: "M_RISK_NO_BACKUP_ACK", acked: true },
      { type: "M_RISK_BACKUP", checked: false }, // 再次操作后 noBackupAck 不应复活
    ]);
    // 勾选→取消路径：checked=true 曾清 ack；此处从未勾回 true，ack 保持用户所设
    const s2 = run(
      [
        { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: true },
        { type: "M_RISK_NO_BACKUP_ACK", acked: true },
        { type: "M_RISK_BACKUP", checked: true }, // 勾回备份 → 二次确认作废
        { type: "M_RISK_BACKUP", checked: false }, // 再取消 → 仍需重新确认
        { type: "M_RISK_CONFIRM" },
      ],
      createInitialManageState(),
    );
    expect(s.risk?.noBackupAck).toBe(true);
    expect(s2.status).toBe("MANAGE"); // 取消备份后旧 ack 作废 → 确认被拒
  });

  it("取消不触发：M_RISK_CANCEL → 回 MANAGE、风险卡清空", () => {
    const s = run([
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: true },
      { type: "M_RISK_CANCEL" },
    ]);
    expect(s.status).toBe("MANAGE");
    expect(s.risk).toBeNull();
    expect(s.op).toBeNull();
  });

  it("风险卡只在 MANAGE 态可开；操作态中 RISK_OPEN 被拒", () => {
    const s = run([
      { type: "M_BEGIN_UPDATE", appId: "jan" },
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: true },
    ]);
    expect(s.risk).toBeNull();
    expect(s.status).toBe("UPDATE");
  });

  it("RISK_CONFIRM 无风险卡（risk=null）→ 拒绝", () => {
    const s = run([{ type: "M_RISK_CONFIRM" }]);
    expect(s.status).toBe("MANAGE");
  });
});

describe("状态机全分支", () => {
  it("UPDATE：MANAGE → UPDATE → DONE", () => {
    let s = run([{ type: "M_BEGIN_UPDATE", appId: "jan" }]);
    expect(s.status).toBe("UPDATE");
    expect(s.appId).toBe("jan");
    s = manageReduce(s, { type: "M_TASKS_READY", tasks: [task("a"), task("b")] });
    s = manageReduce(s, { type: "M_TASK_PHASE", taskId: "a", phase: "verify" });
    expect(s.tasks[0].status).toBe("running");
    expect(s.currentTaskIndex).toBe(0);
    s = manageReduce(s, { type: "M_TASK_DONE", taskId: "a" });
    expect(s.tasks[0].status).toBe("done");
    expect(s.currentTaskIndex).toBe(0); // 指向刚完成的任务（与安装 reducer 同语义）
    s = manageReduce(s, { type: "M_TASK_DONE", taskId: "b" });
    s = manageReduce(s, { type: "M_ALL_DONE" });
    expect(s.status).toBe("DONE");
  });

  it("M_ALL_DONE 有未完成任务 → 拒绝（防提前完成）", () => {
    let s = run([{ type: "M_BEGIN_UPDATE", appId: "jan" }]);
    s = manageReduce(s, { type: "M_TASKS_READY", tasks: [task("a"), task("b", "running")] });
    s = manageReduce(s, { type: "M_ALL_DONE" });
    expect(s.status).toBe("UPDATE");
    // 空任务列表也不推进
    const empty = run([{ type: "M_BEGIN_UPDATE", appId: "jan" }]);
    expect(manageReduce(empty, { type: "M_ALL_DONE" }).status).toBe("UPDATE");
  });

  it("REMOVE：确认 → REMOVE → 卸载完成 → DONE（带备份位置）", () => {
    let s = run([
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: true },
      { type: "M_RISK_CONFIRM" },
    ]);
    expect(s.status).toBe("REMOVE");
    s = manageReduce(s, {
      type: "M_REMOVE_DONE",
      backupDir: "/home/u/.aibox/backup/20261113/jan",
      removed: ["~/AppData/Roaming/Jan"],
      skipped: [{ path: "~/x", reason: "not found" }],
    });
    expect(s.status).toBe("DONE");
    expect(s.backupDir).toContain("backup");
    expect(s.removed).toHaveLength(1);
    expect(s.skipped[0].reason).toBe("not found");
  });

  it("REINSTALL：确认 → 卸载完成不进 DONE（等安装链）→ M_ALL_DONE → DONE", () => {
    let s = run([
      { type: "M_RISK_OPEN", op: "REINSTALL", appId: "jan", backupFirst: true },
      { type: "M_RISK_CONFIRM" },
    ]);
    expect(s.status).toBe("REINSTALL");
    s = manageReduce(s, { type: "M_REMOVE_DONE", backupDir: "/b", removed: ["p"] });
    expect(s.status).toBe("REINSTALL"); // 重装：卸载后还需跑安装链
    expect(s.backupDir).toBe("/b");
    s = manageReduce(s, { type: "M_TASKS_READY", tasks: [task("a")] });
    s = manageReduce(s, { type: "M_TASK_DONE", taskId: "a" });
    s = manageReduce(s, { type: "M_ALL_DONE" });
    expect(s.status).toBe("DONE");
  });

  it("ERROR：操作态任意失败 → ERROR；M_RETRY 回原态；M_CLOSE 回 MANAGE", () => {
    let s = run([{ type: "M_BEGIN_UPDATE", appId: "jan" }]);
    s = manageReduce(s, { type: "M_ERROR", code: "NET_ALL_CHANNELS_DEAD", messageKey: "err.netDead" });
    expect(s.status).toBe("ERROR");
    expect(s.error?.from).toBe("UPDATE");
    expect(s.error?.code).toBe("NET_ALL_CHANNELS_DEAD");
    s = manageReduce(s, { type: "M_RETRY" });
    expect(s.status).toBe("UPDATE");
    expect(s.error).toBeNull();
    // DONE → M_CLOSE → MANAGE（复位）
    const done = run([
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: true },
      { type: "M_RISK_CONFIRM" },
      { type: "M_REMOVE_DONE", removed: [] },
    ]);
    const closed = manageReduce(done, { type: "M_CLOSE" });
    expect(closed.status).toBe("MANAGE");
    expect(closed.appId).toBeNull();
    expect(closed.backupDir).toBeNull();
  });

  it("M_ERROR 只在操作态生效；MANAGE/DONE 中被拒", () => {
    const s = run([{ type: "M_ERROR", code: "X", messageKey: "err.generic" }]);
    expect(s.status).toBe("MANAGE");
  });

  it("M_RETRY 只在 ERROR 生效", () => {
    const s = run([{ type: "M_RETRY" }]);
    expect(s.status).toBe("MANAGE");
  });

  it("M_TASK_PHASE/M_TASK_DONE 只在 UPDATE/REINSTALL 生效", () => {
    const s = run([
      { type: "M_RISK_OPEN", op: "REMOVE", appId: "jan", backupFirst: true },
      { type: "M_RISK_CONFIRM" },
      { type: "M_TASK_PHASE", taskId: "x", phase: "download" },
    ]);
    expect(s.tasks).toHaveLength(0);
    expect(s.status).toBe("REMOVE");
  });

  it("M_REMOVE_DONE 在 UPDATE 态被拒", () => {
    const s = run([
      { type: "M_BEGIN_UPDATE", appId: "jan" },
      { type: "M_REMOVE_DONE", removed: [] },
    ]);
    expect(s.status).toBe("UPDATE");
  });

  it("M_OPEN 复位任意态 → 初始态", () => {
    const s = run([
      { type: "M_BEGIN_UPDATE", appId: "jan" },
      { type: "M_ERROR", code: "X", messageKey: "err.generic" },
      { type: "M_OPEN" },
    ]);
    expect(s).toEqual(createInitialManageState());
  });
});

describe("路径清单组装（硬规则④：只来自 registry maintenance）", () => {
  const m: MaintenanceInfo = {
    dataPaths: ["~/.config/jan", "~/AppData/Roaming/Jan"],
    uninstall: {
      steps: ["Windows: Settings → Jan → uninstall"],
      residualPaths: ["~/AppData/Roaming/Jan"],
    },
    risk: { onUninstall: "本地模型将被删除 / Models deleted", backupFirst: true },
  };

  it("组装 dataPaths/residualPaths/steps/risk 全来自 registry", () => {
    const r = buildRemovePaths(m);
    expect(r.dataPaths).toEqual(m.dataPaths);
    expect(r.residualPaths).toEqual(m.uninstall!.residualPaths);
    expect(r.steps).toEqual(m.uninstall!.steps);
    expect(r.backupFirst).toBe(true);
    expect(r.riskText).toContain("本地模型");
  });

  it("缺省：空清单 + 备份默认开（backupFirst 缺省 true）", () => {
    const r = buildRemovePaths(undefined);
    expect(r.dataPaths).toEqual([]);
    expect(r.residualPaths).toEqual([]);
    expect(r.steps).toEqual([]);
    expect(r.backupFirst).toBe(true);
    expect(r.riskText).toBeUndefined();
    expect(buildRemovePaths(null).backupFirst).toBe(true);
    expect(buildRemovePaths({ risk: { backupFirst: false } }).backupFirst).toBe(false);
  });

  it("组装结果是拷贝（调用方改动不污染 registry 数据）", () => {
    const r = buildRemovePaths(m);
    r.dataPaths.push("/evil/path");
    expect(m.dataPaths).toHaveLength(2);
  });
});
