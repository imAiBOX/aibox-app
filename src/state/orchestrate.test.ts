/**
 * 编排层测试（交付物）：真分支用注入 fake api 驱动四阶段状态推进；
 * mock 回落保持场景注入行为；取消/停滞/错误码映射。
 */
import { describe, expect, it, vi } from "vitest";
import {
  startExecute,
  messageKeyForCode,
  canSwitchSource,
  errorCodeOf,
  type ExecuteApi,
  type ExecuteDeps,
} from "./orchestrate";
import { createInitialState, reduce } from "./reducer";
import { sqlitePersistence, type SqlDriver } from "./persistence-sqlite";
import type { Event, MachineState, Plan, SessionSnapshot, Task, TaskSpec } from "./types";
import type { AiboxEvent } from "../net/client";

const SPEC: TaskSpec = {
  download: {
    id: "t1",
    channels: [{ type: "direct", url: "https://example.com/a.bin" }],
    sha256: "0".repeat(64),
    destName: "a.bin",
  },
  exec: { id: "t1", kind: "nsis-silent", exe: "~/.aibox/cache/downloads/a.bin", args: ["/S"] },
  probe: { cmd: "a --version", expect: "1\\.0" },
};

function task(id = "t1", status: Task["status"] = "pending"): Task {
  return { id, name: id, version: "1.0", phase: "download", status, spec: SPEC };
}

interface Harness {
  deps: ExecuteDeps;
  events: Event[];
  snapshots: SessionSnapshot[];
  emit: (e: AiboxEvent) => void;
  api: ExecuteApi;
}

/** 构造假依赖：事件总线为内存 Map；api 行为可注入 */
function harness(overrides: {
  api?: Partial<ExecuteApi>;
  scenario?: string;
  stallMs?: number;
  phaseTimeouts?: Partial<{ verify: number; install: number; probe: number }>;
}): Harness {
  const listeners = new Set<(e: AiboxEvent) => void>();
  const events: Event[] = [];
  const snapshots: SessionSnapshot[] = [];
  const api: ExecuteApi = {
    downloadStart: vi.fn(async () => "t1"),
    downloadCancel: vi.fn(async () => undefined),
    verifySha256: vi.fn(async () => true),
    execInstall: vi.fn(async () => ({ code: 0, out: "ok" })),
    probeRun: vi.fn(async () => ({ version: "1.0" })),
    ...overrides.api,
  };
  const deps: ExecuteDeps = {
    mode: "real",
    api,
    dispatch: (e) => events.push(e),
    onEvent: (h) => {
      listeners.add(h);
      return () => listeners.delete(h);
    },
    sleep: () => Promise.resolve(),
    scenario: () => overrides.scenario ?? "normal",
    saveSnapshot: async (s) => {
      snapshots.push(structuredClone(s));
    },
    now: () => Date.now(),
    ...(overrides.phaseTimeouts ? { phaseTimeouts: overrides.phaseTimeouts } : {}),
    ...(overrides.stallMs !== undefined ? { stallMs: overrides.stallMs } : {}),
  };
  const emit = (e: AiboxEvent) => {
    for (const h of [...listeners]) h(e);
  };
  return { deps, events, snapshots, emit, api };
}

const last = <T,>(arr: T[]): T => arr[arr.length - 1];

const ok = (text: string): AiboxEvent => ({ event: "log_line", payload: { text } });

describe("真分支四阶段推进（注入 fake api）", () => {
  it("download→verify→install→probe→TASK_DONE→ALL_DONE + 每任务快照", async () => {
    const h = harness({});
    // download_start 前订阅已建立：startExecute 内先订阅再调 api
    // 模拟 Rust 终态 log 在 downloadStart resolve 后立即到达
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(() => h.emit(ok("download ok: 10 bytes via direct (a.bin)")), 0);
      return "t1";
    });
    const handle = startExecute(h.deps, { appId: "app", tasks: [task()] });
    await handle.promise;

    expect(h.events.map((e) => e.type)).toEqual([
      "TASK_PHASE", // download
      "TASK_PHASE", // verify
      "TASK_PHASE", // install
      "TASK_INSTALLER_SUCCEEDED",
      "TASK_PHASE", // probe
      "TASK_DONE",
      "ALL_DONE",
    ]);
    expect(h.api.verifySha256).toHaveBeenCalledWith(
      `~/.aibox/cache/downloads/${"0".repeat(64)}.bin`,
      "0".repeat(64),
    );
    expect(h.api.execInstall).toHaveBeenCalledTimes(1);
    expect(h.api.probeRun).toHaveBeenCalledTimes(1);
    expect(h.events).toContainEqual({ type: "TASK_DONE", taskId: "t1", probeStatus: "version", actualVersion: "1.0" });
    expect(h.snapshots).toEqual([
      { appId: "app", doneTaskIds: [], installedTaskIds: ["t1"], savedAt: expect.any(Number) },
      { appId: "app", doneTaskIds: ["t1"], savedAt: expect.any(Number) },
    ]);
    // 阶段顺序
    const phases = h.events
      .filter((e): e is Extract<Event, { type: "TASK_PHASE" }> => e.type === "TASK_PHASE")
      .map((e) => e.phase);
    expect(phases).toEqual(["download", "verify", "install", "probe"]);
  });

  it.each([
    { packageVersion: "1.24.4", version: "1.24.3", error: "PROBE_WRONG_VERSION" },
    { packageVersion: "1.24.3", version: "1.24.3", error: "PROBE_WRONG_VERSION" },
    { packageVersion: "1.24.4", version: null, error: "PROBE_VERSION_UNCONFIRMED" },
    { packageVersion: "1.24.4", version: "installed", error: "PROBE_VERSION_UNCONFIRMED" },
  ])("upgrade to 1.24.4 with $version on route $packageVersion stops", async ({ packageVersion, version, error }) => {
    const h = harness({ api: { probeRun: vi.fn(async () => ({ version })) } });
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(() => h.emit(ok("download ok: 10 bytes")), 0);
      return "t1";
    });
    const target: Task = { ...task(), role: "target", version: packageVersion, catalogVersion: "1.24.4" };
    await startExecute(h.deps, { appId: "chatbox", tasks: [target], upgradeMode: true }).promise;
    expect(last(h.events)).toMatchObject({ type: "ERROR", code: error, taskId: target.id });
    expect(h.events.some((e) => e.type === "TASK_DONE" || e.type === "ALL_DONE")).toBe(false);
    expect(h.snapshots).toEqual([
      { appId: "chatbox", doneTaskIds: [], installedTaskIds: [target.id], upgradeMode: true, savedAt: expect.any(Number) },
    ]);
    expect(h.events).toContainEqual({ type: "TASK_INSTALLER_SUCCEEDED", taskId: target.id });
    expect(h.api.execInstall).toHaveBeenCalledOnce();
  });

  it("stops before any download or install when the target lacks a probe", async () => {
    const h = harness({});
    const declared = task();
    if (!declared.spec) throw new Error("expected an executable test task");
    const target: Task = { ...declared, role: "target", spec: { ...declared.spec, probe: undefined } };
    await startExecute(h.deps, { appId: "app", tasks: [target] }).promise;
    expect(last(h.events)).toMatchObject({ type: "ERROR", code: "PROBE_VERSION_UNCONFIRMED", taskId: target.id });
    expect(h.api.downloadStart).not.toHaveBeenCalled();
    expect(h.api.execInstall).not.toHaveBeenCalled();
    expect(h.snapshots).toHaveLength(0);
  });

  it("records the actual package version on fresh installs even when the catalog is newer", async () => {
    const h = harness({ api: { probeRun: vi.fn(async () => ({ version: "1.24.3" })) } });
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(() => h.emit(ok("download ok: 10 bytes")), 0);
      return "t1";
    });
    const target: Task = { ...task(), role: "target", version: "1.24.3", catalogVersion: "1.24.4" };
    await startExecute(h.deps, { appId: "chatbox", tasks: [target] }).promise;
    expect(h.events).toContainEqual({ type: "TASK_DONE", taskId: target.id, actualVersion: "1.24.3", probeStatus: "version" });
    expect(last(h.events)).toEqual({ type: "ALL_DONE" });
  });

  // 存在性探测（expect=installed/True 的结构检查）不产版本号——常规安装必须放行，
  // 否则 zed/goose 这类 .app 应用装成功也永远 PROBE_VERSION_UNCONFIRMED（2026-11-28 真机实锤）。
  it("fresh install with a presence probe completes without a version", async () => {
    const h = harness({ api: { probeRun: vi.fn(async () => ({ version: null })) } });
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(() => h.emit(ok("download ok: 10 bytes")), 0);
      return "t1";
    });
    const target: Task = { ...task(), role: "target", version: "1.30.2", catalogVersion: "1.30.2" };
    await startExecute(h.deps, { appId: "zed", tasks: [target] }).promise;
    expect(h.events).toContainEqual({ type: "TASK_DONE", taskId: target.id, probeStatus: "present" });
    expect(last(h.events)).toEqual({ type: "ALL_DONE" });
  });

  it("re-probes a resumed target before accepting the snapshot as an upgrade", async () => {
    const h = harness({ api: { probeRun: vi.fn(async () => ({ version: "1.24.3" })) } });
    const target: Task = { ...task("target", "done"), role: "target", version: "1.24.4", catalogVersion: "1.24.4" };
    await startExecute(h.deps, { appId: "chatbox", tasks: [target], upgradeMode: true }).promise;
    expect(h.api.downloadStart).not.toHaveBeenCalled();
    expect(h.api.probeRun).toHaveBeenCalledOnce();
    expect(last(h.events)).toMatchObject({ type: "ERROR", code: "PROBE_WRONG_VERSION" });
  });

  it.each(["retry", "restart"] as const)("%s after installer success only probes and retains the durable upgrade boundary", async (recovery) => {
    let stored: string | null = null;
    const driver: SqlDriver = {
      async execute(query, values = []) {
        if (query.startsWith("INSERT INTO sessions")) stored = String(values[1]);
      },
      async select<T>() {
        return (stored === null ? [] : [{ state_json: stored }]) as T[];
      },
    };
    const persistence = sqlitePersistence(driver);
    const target: Task = { ...task(), role: "target", version: "1.24.4", catalogVersion: "1.24.4" };
    const plan: Plan = {
      appId: "chatbox", tasks: [target], nodes: [], route: "desktop", channel: "direct", diskMb: 1, estMinutes: 1,
    };
    let state: MachineState = { ...createInitialState(), status: "EXECUTE", appId: plan.appId, plan, tasks: plan.tasks, upgradeMode: true };
    const h = harness({});
    const recordEvent = h.deps.dispatch;
    h.deps.dispatch = (event) => {
      recordEvent(event);
      state = reduce(state, event);
    };
    h.deps.saveSnapshot = async (snapshot) => {
      h.snapshots.push(structuredClone(snapshot));
      await persistence.saveSnapshot(snapshot);
    };
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(() => h.emit(ok("download ok: 10 bytes")), 0);
      return target.id;
    });
    h.api.probeRun = vi.fn(async () => {
      // The installer boundary must already be serialized when probing begins.
      expect(await persistence.loadSession()).toEqual({
        appId: plan.appId, doneTaskIds: [], installedTaskIds: [target.id], upgradeMode: true, savedAt: expect.any(Number),
      });
      throw { code: "PROBE_MISMATCH" };
    });
    await startExecute(h.deps, { appId: state.appId, tasks: state.tasks, upgradeMode: state.upgradeMode }).promise;
    expect(state.status).toBe("ERROR");
    expect(state.tasks[0]).toMatchObject({ status: "failed", installerSucceeded: true });
    expect(last(h.events)).toMatchObject({ type: "ERROR", code: "PROBE_MISMATCH" });

    if (recovery === "retry") {
      state = reduce(state, { type: "RETRY" });
    } else {
      // A fresh persistence instance reads the SQL JSON written by the first run.
      const snapshot = await sqlitePersistence(driver).loadSession();
      expect(snapshot).not.toBeNull();
      state = reduce(createInitialState(), { type: "DETECT_SESSION", snapshot: snapshot! });
      state = reduce(state, { type: "RESUME_PLAN", plan });
      state = reduce(state, { type: "RESUME_SESSION" });
    }
    expect(state).toMatchObject({ status: "EXECUTE", upgradeMode: true });
    expect(state.tasks[0].installerSucceeded).toBe(true);
    h.events.length = 0;
    h.api.probeRun = vi.fn(async () => ({ version: "1.24.4" }));
    await startExecute(h.deps, { appId: state.appId, tasks: state.tasks, upgradeMode: state.upgradeMode }).promise;

    expect(h.api.downloadStart).toHaveBeenCalledOnce();
    expect(h.api.verifySha256).toHaveBeenCalledOnce();
    expect(h.api.execInstall).toHaveBeenCalledOnce();
    expect(h.api.probeRun).toHaveBeenCalledOnce();
    expect(h.events).toEqual([
      { type: "TASK_PHASE", taskId: target.id, phase: "probe" },
      { type: "TASK_DONE", taskId: target.id, actualVersion: "1.24.4", probeStatus: "version" },
      { type: "ALL_DONE" },
    ]);
    expect(state.status).toBe("DONE"); // Upgrades finish without reconfiguring the application.
    expect(await sqlitePersistence(driver).loadSession()).toEqual({
      appId: plan.appId, doneTaskIds: [target.id], upgradeMode: true, savedAt: expect.any(Number),
    });
  });

  it("多任务顺序执行；done/skipped 跳过（断点续装语义）", async () => {
    const h = harness({});
    h.api.downloadStart = vi.fn(async (t) => {
      setTimeout(() => h.emit(ok(`download ok: 1 bytes via x (${t.destName})`)), 0);
      return t.id;
    });
    const tasks = [task("a", "done"), task("b", "skipped"), task("c"), task("d")];
    const handle = startExecute(h.deps, { appId: "app", tasks });
    await handle.promise;
    expect(h.api.downloadStart).toHaveBeenCalledTimes(2); // c, d
    expect(h.api.downloadStart).toHaveBeenNthCalledWith(1, SPEC.download);
    const dones = h.events.filter((e) => e.type === "TASK_DONE");
    expect(dones).toHaveLength(2);
    // Skips must never become completions on restart.
    expect(h.snapshots).toHaveLength(4);
    expect(last(h.snapshots).doneTaskIds).toEqual(["a", "c", "d"]);
  });

  it("下载失败 log → ERROR 带 human_error_code + taskId", async () => {
    const h = harness({});
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(
        () => h.emit(ok("download failed [NET_ALL_CHANNELS_DEAD]: all sources dead")),
        0,
      );
      return "t1";
    });
    const handle = startExecute(h.deps, { appId: "app", tasks: [task()] });
    await handle.promise;
    // 先 TASK_PHASE(download)，再 ERROR
    expect(h.events).toHaveLength(2);
    expect(h.events[0]).toMatchObject({ type: "TASK_PHASE", phase: "download" });
    expect(h.events[1]).toEqual({
      type: "ERROR",
      code: "NET_ALL_CHANNELS_DEAD",
      messageKey: "err.netDead",
      taskId: "t1",
    });
    expect(h.api.verifySha256).not.toHaveBeenCalled();
  });

  it("verify 返回 false → HASH_MISMATCH（C 档篡改语义）", async () => {
    const h = harness({ api: { verifySha256: vi.fn(async () => false) } });
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(() => h.emit(ok("download ok: 1 bytes via x (a.bin)")), 0);
      return "t1";
    });
    await startExecute(h.deps, { appId: "app", tasks: [task()] }).promise;
    expect(last(h.events)).toMatchObject({ type: "ERROR", code: "HASH_MISMATCH" });
    expect(h.api.execInstall).not.toHaveBeenCalled();
  });

  it("exec exit≠0 → EXEC_EXIT_n；probe 抛错 → 错误码透传", async () => {
    const h1 = harness({ api: { execInstall: vi.fn(async () => ({ code: 5, out: "boom" })) } });
    h1.api.downloadStart = vi.fn(async () => {
      setTimeout(() => h1.emit(ok("download ok: 1 via x (a.bin)")), 0);
      return "t1";
    });
    await startExecute(h1.deps, { appId: "app", tasks: [task()] }).promise;
    expect(last(h1.events)).toMatchObject({ type: "ERROR", code: "EXEC_EXIT_5" });

    const h2 = harness({
      api: { probeRun: vi.fn(async () => Promise.reject({ code: "PROBE_MISMATCH" })) },
    });
    h2.api.downloadStart = vi.fn(async () => {
      setTimeout(() => h2.emit(ok("download ok: 1 via x (a.bin)")), 0);
      return "t1";
    });
    await startExecute(h2.deps, { appId: "app", tasks: [task()] }).promise;
    expect(last(h2.events)).toMatchObject({
      type: "ERROR",
      code: "PROBE_MISMATCH",
      messageKey: "err.probeFailed",
    });
  });

  it("download_start reject（invoke 错误 {code}）→ 错误码映射", async () => {
    const h = harness({
      api: { downloadStart: vi.fn(async () => Promise.reject({ code: "DOWNLOAD_BUSY" })) },
    });
    await startExecute(h.deps, { appId: "app", tasks: [task()] }).promise;
    expect(last(h.events)).toMatchObject({ type: "ERROR", code: "DOWNLOAD_BUSY" });
  });

  it("无 spec 的任务 → TASK_NO_SPEC（防真分支空转）", async () => {
    const h = harness({});
    const bare: Task = { id: "x", name: "x", version: "1", phase: "download", status: "pending" };
    await startExecute(h.deps, { appId: "app", tasks: [bare] }).promise;
    expect(last(h.events)).toMatchObject({ type: "ERROR", code: "TASK_NO_SPEC" });
  });

  it("停滞 watchdog：无事件超过 stallMs → DOWNLOAD_STALLED", async () => {
    const h = harness({ stallMs: 20 });
    h.api.downloadStart = vi.fn(async () => "t1"); // 永不发终态
    await startExecute(h.deps, { appId: "app", tasks: [task()] }).promise;
    expect(last(h.events)).toMatchObject({
      type: "ERROR",
      code: "DOWNLOAD_STALLED",
      messageKey: "err.downloadFailed",
    });
  }, 5000);

  it("stop()：取消在途下载 + 不派发 ERROR", async () => {
    const h = harness({});
    h.api.downloadStart = vi.fn(async () => "t1"); // 立即返回 jobId，但永不到终态 log
    const handle = startExecute(h.deps, { appId: "app", tasks: [task()] });
    // 等进入 waitDownload（downloadStart 已 resolve）
    await vi.waitFor(() => expect(h.api.downloadStart).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 5));
    handle.stop();
    await handle.promise;
    expect(h.api.downloadCancel).toHaveBeenCalledWith("t1");
    expect(h.events.every((e) => e.type !== "ERROR")).toBe(true);
  });

  it("download_finished 结构化终态可独立驱动（不依赖 log_line 文本）", async () => {
    const h = harness({});
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(
        () => h.emit({ event: "download_finished", payload: { ok: true, jobId: "t1", bytes: 10, channel: "direct" } }),
        0,
      );
      return "t1";
    });
    const handle = startExecute(h.deps, { appId: "app", tasks: [task()] });
    await handle.promise;
    expect(h.events.map((e) => e.type)).toEqual([
      "TASK_PHASE",
      "TASK_PHASE",
      "TASK_PHASE",
      "TASK_INSTALLER_SUCCEEDED",
      "TASK_PHASE",
      "TASK_DONE",
      "ALL_DONE",
    ]);
  });

  it("download_finished 失败码 → ERROR 事件透传（结构化错误分支）", async () => {
    const h = harness({});
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(
        () => h.emit({ event: "download_finished", payload: { ok: false, jobId: "t1", code: "NET_HASH_MISMATCH" } }),
        0,
      );
      return "t1";
    });
    const handle = startExecute(h.deps, { appId: "app", tasks: [task()] });
    await handle.promise;
    expect(h.events).toHaveLength(2);
    expect(h.events[0]).toMatchObject({ type: "TASK_PHASE", phase: "download" });
    expect(h.events[1]).toMatchObject({ type: "ERROR", code: "NET_HASH_MISMATCH", taskId: "t1" });
  });
});

describe("mock 回落（行为保持）", () => {
  it("normal：四阶段定时器推进 → TASK_DONE → ALL_DONE", async () => {
    const h = harness({ scenario: "normal" });
    h.deps.mode = "mock";
    await startExecute(h.deps, { appId: "app", tasks: [task()] }).promise;
    expect(h.events.map((e) => e.type)).toEqual([
      "TASK_PHASE",
      "TASK_PHASE",
      "TASK_PHASE",
      "TASK_PHASE",
      "TASK_DONE",
      "ALL_DONE",
    ]);
    expect(h.api.downloadStart).not.toHaveBeenCalled(); // mock 不走 api
  });

  it("场景 net_dead → ERROR NET_ALL_CHANNELS_DEAD（download 阶段注入）", async () => {
    const h = harness({ scenario: "net_dead" });
    h.deps.mode = "mock";
    await startExecute(h.deps, { appId: "app", tasks: [task()] }).promise;
    expect(last(h.events)).toMatchObject({
      type: "ERROR",
      code: "NET_ALL_CHANNELS_DEAD",
      messageKey: "err.netDead",
      taskId: "t1",
    });
  });

  it("场景 hash_mismatch → ERROR HASH_MISMATCH（verify 阶段注入）", async () => {
    const h = harness({ scenario: "hash_mismatch" });
    h.deps.mode = "mock";
    await startExecute(h.deps, { appId: "app", tasks: [task()] }).promise;
    expect(last(h.events)).toMatchObject({ type: "ERROR", code: "HASH_MISMATCH" });
  });
});

describe("messageKeyForCode", () => {
  it("已知码 → 人话 key；未知码 → err.generic", () => {
    expect(messageKeyForCode("NET_ALL_CHANNELS_DEAD")).toBe("err.netDead");
    expect(messageKeyForCode("NET_HASH_MISMATCH")).toBe("err.hashMismatch");
    expect(messageKeyForCode("HASH_MISMATCH")).toBe("err.hashMismatch");
    expect(messageKeyForCode("EXEC_EXIT_1")).toBe("err.execFailed");
    expect(messageKeyForCode("PROBE_MISMATCH")).toBe("err.probeFailed");
    expect(messageKeyForCode("DOWNLOAD_STALLED")).toBe("err.downloadFailed");
    expect(messageKeyForCode("VERIFY_FILE_MISSING")).toBe("err.downloadFailed");
    expect(messageKeyForCode("USER_CANCELLED")).toBe("err.cancelled");
    expect(messageKeyForCode("WEIRD_CODE")).toBe("err.generic");
  });

  it("配置写入类错误：按原因分流（用户要知道原文件有没有被动过）", () => {
    expect(messageKeyForCode("CONFIG_PARSE")).toBe("err.configParse");
    expect(messageKeyForCode("CONFIG_NO_VALUES")).toBe("err.configNoValues");
    expect(messageKeyForCode("CONFIG_SERIALIZE")).toBe("err.configFailed");
    expect(messageKeyForCode("CONFIG_BAD_PROVIDER")).toBe("err.configFailed");
    expect(messageKeyForCode("CONFIG_NO_APP")).toBe("err.configFailed");
    expect(messageKeyForCode("IO_ERROR")).toBe("err.configIo");
  });

  it("模型测试类错误：精确码优先于 VERIFY_ 前缀", () => {
    expect(messageKeyForCode("VERIFY_HTTP_401")).toBe("err.verifyKey");
    expect(messageKeyForCode("VERIFY_HTTP_403")).toBe("err.verifyKey");
    expect(messageKeyForCode("VERIFY_NET_FAIL")).toBe("err.verifyNet");
    expect(messageKeyForCode("VERIFY_EMPTY_TEXT")).toBe("err.verifyWeak");
    expect(messageKeyForCode("VERIFY_NO_KEY")).toBe("err.verifyNoKey");
    expect(messageKeyForCode("VERIFY_UNSUPPORTED_PROVIDER")).toBe("err.verifyUnsupported");
    expect(messageKeyForCode("VERIFY_BAD_RESPONSE")).toBe("err.verifyBadResponse");
    expect(messageKeyForCode("VERIFY_HTTP_500")).toBe("err.verifyFailed");
    // 下载期的 SHA 校验仍走下载类文案（VERIFY_ 前缀被两个流程共用）
    expect(messageKeyForCode("VERIFY_FILE_MISSING")).toBe("err.downloadFailed");
  });

  it("精确码不做前缀匹配：VERIFY_HTTP_4010 不能被当成 401（REJECT-2 回归）", () => {
    expect(messageKeyForCode("VERIFY_HTTP_401")).toBe("err.verifyKey");
    // 更长的码不是 401 → 不得说成「key 无效」
    expect(messageKeyForCode("VERIFY_HTTP_4010")).not.toBe("err.verifyKey");
    expect(messageKeyForCode("VERIFY_HTTP_4010")).toBe("err.verifyFailed");
    expect(messageKeyForCode("VERIFY_HTTP_429")).toBe("err.verifyFailed");
  });

  it("registry 拒绝类错误与「网络不通」分开说（防篡改类不该让用户查网络）", () => {
    expect(messageKeyForCode("REGISTRY_MISMATCH")).toBe("err.registryMismatch");
    expect(messageKeyForCode("REGISTRY_INTEGRITY")).toBe("err.registryIntegrity");
    expect(messageKeyForCode("REGISTRY_SINGLE_SOURCE")).toBe("err.registrySingleSource");
    // 可用性类 → 仍是「检查网络」
    expect(messageKeyForCode("REGISTRY_UNAVAILABLE")).toBe("err.registry");
    expect(messageKeyForCode("REGISTRY_NO_CACHE")).toBe("err.registry");
    expect(messageKeyForCode("REGISTRY_FETCH")).toBe("err.registry");
  });
});

describe("errorCodeOf", () => {
  it("取 Rust CommandError 的 code；非结构化错误 → fallback", () => {
    expect(errorCodeOf({ code: "REGISTRY_MISMATCH", human: "x" }, "REGISTRY_FETCH")).toBe(
      "REGISTRY_MISMATCH",
    );
    expect(errorCodeOf(new Error("boom"), "REGISTRY_FETCH")).toBe("REGISTRY_FETCH");
    expect(errorCodeOf(undefined, "REGISTRY_FETCH")).toBe("REGISTRY_FETCH");
    expect(errorCodeOf({ code: 42 }, "REGISTRY_FETCH")).toBe("REGISTRY_FETCH");
  });
});

describe("换源重发（skipChannels）", () => {
  const okStart = (h: Harness) =>
    vi.fn(async (t: { destName: string; id: string }) => {
      setTimeout(() => h.emit(ok(`download ok: 1 bytes via x (${t.destName})`)), 0);
      return t.id;
    });

  it("takeSkipChannels 返回的通道作为 skipChannels 传给 downloadStart", async () => {
    const h = harness({});
    h.api.downloadStart = okStart(h);
    const take = vi.fn(() => ["PROXY"]);
    h.deps.takeSkipChannels = take;

    await startExecute(h.deps, { appId: "app", tasks: [task()] }).promise;

    expect(take).toHaveBeenCalledTimes(1);
    expect(h.api.downloadStart).toHaveBeenCalledWith({
      ...SPEC.download,
      skipChannels: ["PROXY"],
    });
    expect(last(h.events)).toMatchObject({ type: "ALL_DONE" });
  });

  it("skipChannels 只消费一次（第二个任务不再带 skip）", async () => {
    const h = harness({});
    h.api.downloadStart = okStart(h);
    // 模拟 App 的一次性消费（取后清空）
    let pending: string[] = ["PROXY"];
    const take = vi.fn(() => {
      const s = pending;
      pending = [];
      return s;
    });
    h.deps.takeSkipChannels = take;

    await startExecute(h.deps, { appId: "app", tasks: [task("t1"), task("t2")] }).promise;

    expect(take).toHaveBeenCalledTimes(2);
    expect(h.api.downloadStart).toHaveBeenNthCalledWith(1, {
      ...SPEC.download,
      skipChannels: ["PROXY"],
    });
    expect(h.api.downloadStart).toHaveBeenNthCalledWith(2, SPEC.download);
  });

  it("未提供 takeSkipChannels → payload 不带 skipChannels 字段", async () => {
    const h = harness({});
    h.api.downloadStart = okStart(h);
    await startExecute(h.deps, { appId: "app", tasks: [task()] }).promise;
    expect(h.api.downloadStart).toHaveBeenCalledWith(SPEC.download);
    const arg = vi.mocked(h.api.downloadStart).mock.calls[0][0];
    expect("skipChannels" in arg).toBe(false);
  });
});

describe("canSwitchSource（换源按钮展示条件）", () => {
  it("下载类错误可换源；其他不可", () => {
    expect(canSwitchSource("NET_HASH_MISMATCH")).toBe(true);
    expect(canSwitchSource("HASH_MISMATCH")).toBe(true);
    expect(canSwitchSource("DOWNLOAD_STALLED")).toBe(true);
    expect(canSwitchSource("EXEC_EXIT_1")).toBe(false);
    expect(canSwitchSource("SCAN_FAILED")).toBe(false);
    expect(canSwitchSource("REGISTRY_FETCH")).toBe(false);
  });

  it("所有源都超时 → 不给「换个下载源」（已无源可换，只会白等一轮）", () => {
    expect(canSwitchSource("NET_ALL_CHANNELS_DEAD")).toBe(false);
  });
});

it("安装队列完成时不调用旧版统计 IPC", async () => {
  const h = harness({});
  const trackInstall = vi.fn(async () => undefined);
  // 模拟仍含旧版包装函数的 api 对象，防止安装编排重新接上该 IPC。
  Object.assign(h.api, { trackInstall });
  h.api.downloadStart = vi.fn(async (t) => {
    setTimeout(() => h.emit(ok(`download ok: 1 bytes via x (${t.destName})`)), 0);
    return t.id;
  });
  await startExecute(h.deps, {
    appId: "openclaw",
    tasks: [task("a"), task("b")],
  }).promise;
  expect(h.api.downloadStart).toHaveBeenCalledTimes(2);
  expect(last(h.events)).toMatchObject({ type: "ALL_DONE" });
  expect(trackInstall).not.toHaveBeenCalled();
});

describe("阶段看门狗只提示，不取消后端或开放并发重试", () => {
  for (const phase of ["verify", "install", "probe"] as const) {
    it(`${phase} 慢时仅提示，直到后端结束才继续`, async () => {
      let finish!: (value: any) => void;
      const pending = new Promise<any>((r) => { finish = r; });
      const h = harness({ phaseTimeouts: { verify: 5, install: 5, probe: 5 } });
      const slow = vi.fn();
      h.deps.onPhaseSlow = slow;
      if (phase === "verify") h.api.verifySha256 = () => pending;
      if (phase === "install") h.api.execInstall = () => pending;
      if (phase === "probe") h.api.probeRun = () => pending;
      h.api.downloadStart = vi.fn(async (t) => {
        setTimeout(() => h.emit(ok(`download ok: 1 bytes via x (${t.destName})`)), 0);
        return t.id;
      });
      const handle = startExecute(h.deps, { appId: "app", tasks: [task()] });
      await new Promise((r) => setTimeout(r, 25));
      expect(slow).toHaveBeenCalledWith("t1", phase);
      expect(h.events.some((e) => e.type === "ERROR" || e.type === "ALL_DONE")).toBe(false);
      finish(phase === "verify" ? true : phase === "install" ? { code: 0, out: "" } : { version: "1.0" });
      await handle.promise;
      expect(h.events.some((e) => e.type === "ALL_DONE")).toBe(true);
    });
  }

  it("取消后后台迟到的错误不得覆盖新流程", async () => {
    let reject!: (e: unknown) => void;
    const pending = new Promise<boolean>((_, r) => { reject = r; });
    const h = harness({ api: { verifySha256: () => pending }, phaseTimeouts: { verify: 5 } });
    h.api.downloadStart = vi.fn(async (t) => {
      setTimeout(() => h.emit(ok(`download ok: 1 bytes via x (${t.destName})`)), 0);
      return t.id;
    });
    const slow = vi.fn(); h.deps.onPhaseSlow = slow;
    const handle = startExecute(h.deps, { appId: "app", tasks: [task()] });
    await new Promise((r) => setTimeout(r, 3));
    handle.stop(); reject({ code: "VERIFY_FAILED" });
    await handle.promise;
    expect(h.events.some((e) => e.type === "ERROR")).toBe(false);
  });
});
