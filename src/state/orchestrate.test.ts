/**
 * 编排层测试（交付物）：真分支用注入 fake api 驱动四阶段状态推进；
 * mock 回落保持场景注入行为；取消/停滞/错误码映射。
 */
import { describe, expect, it, vi } from "vitest";
import {
  startExecute,
  messageKeyForCode,
  canSwitchSource,
  type ExecuteApi,
  type ExecuteDeps,
} from "./orchestrate";
import type { Event, SessionSnapshot, Task, TaskSpec } from "./types";
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
      snapshots.push(s);
    },
    now: () => Date.now(),
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
      "TASK_PHASE", // probe
      "TASK_DONE",
      "ALL_DONE",
    ]);
    expect(h.api.verifySha256).toHaveBeenCalledWith(
      "~/.aibox/cache/downloads/a.bin",
      "0".repeat(64),
    );
    expect(h.api.execInstall).toHaveBeenCalledTimes(1);
    expect(h.api.probeRun).toHaveBeenCalledTimes(1);
    expect(h.snapshots).toHaveLength(1);
    expect(h.snapshots[0].doneTaskIds).toEqual(["t1"]);
    // 阶段顺序
    const phases = h.events
      .filter((e): e is Extract<Event, { type: "TASK_PHASE" }> => e.type === "TASK_PHASE")
      .map((e) => e.phase);
    expect(phases).toEqual(["download", "verify", "install", "probe"]);
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
    // 快照含进入时已处置任务（a done / b skipped）+ 新完成的 c、d
    expect(h.snapshots[1].doneTaskIds).toEqual(["a", "b", "c", "d"]);
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
    expect(canSwitchSource("NET_ALL_CHANNELS_DEAD")).toBe(true);
    expect(canSwitchSource("HASH_MISMATCH")).toBe(true);
    expect(canSwitchSource("DOWNLOAD_STALLED")).toBe(true);
    expect(canSwitchSource("EXEC_EXIT_1")).toBe(false);
    expect(canSwitchSource("SCAN_FAILED")).toBe(false);
    expect(canSwitchSource("REGISTRY_FETCH")).toBe(false);
  });
});

describe("安装上报（track_install 时机与幂等）", () => {
  it("首个待执行任务前上报一次；多任务不重复", async () => {
    const order: string[] = [];
    const h = harness({});
    h.api.trackInstall = vi.fn(async () => {
      order.push("track");
    });
    h.api.downloadStart = vi.fn(async (t) => {
      order.push("download");
      setTimeout(() => h.emit(ok(`download ok: 1 bytes via x (${t.destName})`)), 0);
      return t.id;
    });
    const handle = startExecute(h.deps, {
      appId: "openclaw",
      tasks: [task("a"), task("b")],
    });
    await handle.promise;
    expect(h.api.trackInstall).toHaveBeenCalledTimes(1); // 同轮幂等
    expect(order[0]).toBe("track"); // 上报先于首个 download_start
    expect(order.filter((x) => x === "download")).toHaveLength(2);
    expect(last(h.events)).toMatchObject({ type: "ALL_DONE" });
  });

  it("上报 reject / 同步抛 → 主流程照常（静默不阻塞）", async () => {
    const drive = async (h: Harness) => {
      h.api.downloadStart = vi.fn(async () => {
        setTimeout(() => h.emit(ok("download ok: 1 bytes via x (a.bin)")), 0);
        return "t1";
      });
      await startExecute(h.deps, { appId: "app", tasks: [task()] }).promise;
      expect(last(h.events)).toMatchObject({ type: "ALL_DONE" });
      expect(h.api.verifySha256).toHaveBeenCalled(); // 四阶段照走
    };

    const h1 = harness({});
    h1.api.trackInstall = vi.fn(async () => {
      throw new Error("net down");
    });
    await drive(h1);
    expect(h1.api.trackInstall).toHaveBeenCalledTimes(1);

    const h2 = harness({});
    h2.api.trackInstall = vi.fn(() => {
      throw new Error("sync boom");
    });
    await drive(h2);
  });

  it("无 appId / 本轮无可执行任务 → 不上报", async () => {
    const h = harness({});
    h.api.trackInstall = vi.fn(async () => undefined);
    h.api.downloadStart = vi.fn(async () => {
      setTimeout(() => h.emit(ok("download ok: 1 bytes via x (a.bin)")), 0);
      return "t1";
    });
    await startExecute(h.deps, { appId: null, tasks: [task()] }).promise;
    expect(h.api.trackInstall).not.toHaveBeenCalled();

    const h2 = harness({});
    h2.api.trackInstall = vi.fn(async () => undefined);
    await startExecute(h2.deps, {
      appId: "app",
      tasks: [task("a", "done"), task("b", "skipped")],
    }).promise;
    expect(h2.api.trackInstall).not.toHaveBeenCalled(); // 全处置 = 无安装动作
  });
});
