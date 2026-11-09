import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { setLang, type Lang } from "./i18n";
import { hotLoadInto } from "./i18n/loader";
import i18n from "./i18n";
import {
  createInitialState,
  createPersistence,
  e1Reduce,
  createE1State,
  reduce,
  screenFor,
  type Plan,
} from "./state";
import { api, getMockScenario, isMock, type AppIndex } from "./net/client";
import { channelToast, feedLogFromEvents, feedToastFromEvents, startTauriEvents } from "./net/events";
import { AppShell, type BarAction } from "./screens/Shell";
import S1Idle from "./screens/S1Idle";
import S2Scan from "./screens/S2Scan";
import S3Plan from "./screens/S3Plan";
import S4Execute from "./screens/S4Execute";
import S5Config from "./screens/S5Config";
import S6Verify from "./screens/S6Verify";
import S7Done from "./screens/S7Done";
import ErrorOverlay from "./screens/ErrorOverlay";
import SettingsNetwork from "./screens/SettingsNetwork";
import SmartScreen from "./screens/SmartScreen";
import { useUpdateBanner } from "./hooks/useUpdateBanner";
import { useSessionResume } from "./hooks/useSessionResume";
import E1DiagPanel from "./screens/E1DiagPanel";
import { runVerify } from "./config/flow";
import type { ConfigCreds } from "./screens/S5Config";

type Overlay = "none" | "settings" | "smartscreen";

export default function App() {
  const { t, i18n: i18next } = useTranslation();
  const [state, rawDispatch] = useReducer(reduce, undefined, createInitialState);
  const dispatch = rawDispatch;
  const [overlay, setOverlay] = useState<Overlay>("none");
  const [version, setVersion] = useState(__APP_VERSION__);
  const [appIndex, setAppIndex] = useState<AppIndex | null>(null);
  const [indexLoading, setIndexLoading] = useState(true);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  /** C5 写入成功后的凭据（内存态，仅供 VERIFY；不进持久化） */
  const [creds, setCreds] = useState<ConfigCreds | null>(null);
  const [verifyResult, setVerifyResult] = useState<{ text: string; ms: number } | null>(null);
  const [verifyError, setVerifyError] = useState<string | null>(null);
  const [verifyPending, setVerifyPending] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  // 断点续装：真 Tauri → SQLite（sessions/kv），mock/浏览器 → memory（persistence-sqlite 工厂）
  const persistence = useRef(createPersistence()).current;
  // E1 内嵌诊断面板（同一任务连败 2 次触发；纯 reducer 可测）
  const [e1, rawE1] = useReducer(e1Reduce, createE1State());
  /** 导出成功后关闭 → 用 EXPORT_DIAG 语义恢复（与 RETRY 同转移，保留事件契约） */
  const exportedRef = useRef(false);
  const banner = useUpdateBanner();
  const resume = useSessionResume(persistence, dispatch, state);

  // 版本号：Tauri 壳优先，失败回落构建期注入
  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => undefined);
  }, []);

  // 启动热加载 registry locales（失败静默回落内置 en）
  useEffect(() => {
    void hotLoadInto(i18n, i18next.resolvedLanguage ?? "en");
  }, [i18next.resolvedLanguage]);

  // 应用目录（IDLE 拉 registry_index；mock 模式即内存数据）
  useEffect(() => {
    let alive = true;
    api
      .registryIndex()
      .then((idx) => alive && setAppIndex(idx))
      .catch(() => alive && setAppIndex({ apps: [] }))
      .finally(() => alive && setIndexLoading(false));
    return () => {
      alive = false;
    };
  }, []);

  // 真事件桥（download_progress/task_phase/channel_switched/log_line → 统一事件总线；mock 已内建推送）
  useEffect(() => {
    const stopBridge = startTauriEvents();
    const offToast = feedToastFromEvents(channelToast, (p) =>
      t("s4.switched", { from: p.from, to: p.to }),
    );
    const offLog = feedLogFromEvents();
    return () => {
      offToast();
      offLog();
      stopBridge();
    };
  }, [t]);

  // 通道切换 toast：3s 自动消失（不打断流程）
  useEffect(() => channelToast.subscribe(setToast), []);

  // E1 触发源：EXECUTE 中同一任务连败 2 次（RETRY 后再败即计 2）
  useEffect(() => {
    if (state.status === "ERROR" && state.error?.from === "EXECUTE" && state.error.taskId) {
      rawE1({ type: "FAIL", taskId: state.error.taskId });
    }
  }, [state.status, state.error]);
  // 任务成功 → 连败链断裂
  useEffect(() => {
    if (state.status !== "EXECUTE") return;
    const cur = state.tasks[state.currentTaskIndex];
    if (cur?.status === "done") rawE1({ type: "SUCCESS", taskId: cur.id });
  }, [state.status, state.tasks, state.currentTaskIndex]);

  // 断点续装写入时机：状态机关键转移（每任务完成在下方 runTask 内单独写）
  useEffect(() => {
    if (!state.appId || state.tasks.length === 0) return;
    const KEY_PHASES = ["EXECUTE", "CONFIG", "VERIFY", "ERROR"] as const;
    if (!(KEY_PHASES as readonly string[]).includes(state.status)) return;
    void persistence.saveSnapshot({
      appId: state.appId,
      doneTaskIds: state.tasks.filter((x) => x.status === "done").map((x) => x.id),
      savedAt: Date.now(),
    });
  }, [state.status, state.tasks, state.appId, persistence]);

  // 完成安装 → 清会话（下次启动不再问「继续上次安装？」）
  useEffect(() => {
    if (state.status === "DONE") void persistence.clearSession();
  }, [state.status, persistence]);

  // ── 主流程编排（mock 数据驱动状态机；换真 invoke）──
  const startInstall = useCallback(() => {
    if (!selectedId) return;
    dispatch({ type: "START_INSTALL" });
    api
      .scanSystem()
      .then((report) => dispatch({ type: "SCAN_DONE", report }))
      .catch((_e: unknown) =>
        dispatch({
          type: "ERROR",
          code: "SCAN_FAILED",
          messageKey: "err.scanFailed",
        }),
      );
    // 扫描期间预取 manifest，PLAN 态直接可用
    api
      .registryApp(selectedId)
      .then((m) => {
        const plan: Plan = {
          appId: m.id,
          nodes: m.tasks.map((task, i) => ({
            id: task.id,
            name: task.name,
            versionRange: task.version,
            kind: i === m.tasks.length - 1 ? ("target" as const) : ("install" as const),
            children: [],
          })).map((n, i, arr) => ({ ...n, children: i + 1 < arr.length ? [arr[i + 1].id] : [] })),
          tasks: m.tasks.map((task) => ({
            id: task.id,
            name: task.name,
            version: task.version,
            phase: "download" as const,
            status: "pending" as const,
          })),
          route: m.routes.global?.[0] ?? "direct",
          channel: "ghfast.top",
          diskMb: 760,
          estMinutes: 6,
        };
        dispatch({ type: "PLAN_READY", plan });
        // 诊断包收录最后计划：写 ~/.aibox/cache/last-plan.json，失败不阻塞
        void api.savePlan(plan).catch(() => undefined);
      })
      .catch(() => dispatch({ type: "ERROR", code: "REGISTRY_FETCH", messageKey: "err.registry" }));
  }, [selectedId]);

  // EXECUTE：mock 阶段推进（每任务 download→verify→install→probe→快照持久化）
  const execRunning = useRef(false);
  useEffect(() => {
    if (state.status !== "EXECUTE" || execRunning.current) return;
    execRunning.current = true;
    let cancelled = false;

    const runTask = async (idx: number): Promise<void> => {
      if (cancelled || idx >= state.tasks.length) return;
      const task = state.tasks[idx];
      const phases = ["download", "verify", "install", "probe"] as const;
      for (const phase of phases) {
        if (cancelled) return;
        // mock 场景失败注入（C 档/B 档分支）：失败 → ERROR（带 taskId，E1 连败计数源）
        if (isMock()) {
          const sc = getMockScenario();
          if (sc === "net_dead" && phase === "download") {
            dispatch({ type: "ERROR", code: "NET_ALL_CHANNELS_DEAD", messageKey: "err.netDead", taskId: task.id });
            return;
          }
          if (sc === "hash_mismatch" && phase === "verify") {
            dispatch({ type: "ERROR", code: "HASH_MISMATCH", messageKey: "err.hashMismatch", taskId: task.id });
            return;
          }
        }
        dispatch({ type: "TASK_PHASE", taskId: task.id, phase });
        await new Promise((r) => setTimeout(r, isMock() ? 180 : 60));
      }
      if (cancelled) return;
      dispatch({ type: "TASK_DONE", taskId: task.id });
      // 断点续装快照（EXECUTE 每任务完成即写；真 Tauri → SQLite sessions 表）
      const snap = {
        appId: state.appId ?? task.id,
        doneTaskIds: state.tasks.slice(0, idx + 1).map((x) => x.id),
        savedAt: Date.now(),
      };
      await persistence.saveSnapshot(snap);
    };

    void (async () => {
      for (let i = 0; i < state.tasks.length; i++) {
        if (cancelled) return;
        // 用最新 tasks 状态判断（跳过的任务不重复执行）
        const current = state.tasks[i];
        if (current.status === "done" || current.status === "skipped") continue;
        await runTask(i);
      }
      if (!cancelled) dispatch({ type: "ALL_DONE" });
      execRunning.current = false;
    })();

    return () => {
      cancelled = true;
      execRunning.current = false;
    };
    // 依赖 state.tasks.length / status；单次进入 EXECUTE 跑一轮
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.status, state.tasks.length]);

  const sendVerify = useCallback(() => {
    setVerifyPending(true);
    setVerifyError(null);
    const started = Date.now();
    // 真实请求：C5 已写入的配置即被真实使用；弱判定（无文本）由 runVerify 拒绝
    runVerify({
      provider: (creds?.provider ?? state.configPathId ?? "openrouter") as ConfigCreds["provider"],
      apiKey: creds?.apiKey,
      model: creds?.model,
    })
      .then((r) => {
        setVerifyPending(false);
        if (r.ok && r.text) {
          setVerifyResult({ text: r.text, ms: r.latencyMs ?? Date.now() - started });
          dispatch({ type: "VERIFY_OK" });
        } else {
          const human = r.errorKey ? t(r.errorKey) : t("s6.failGeneric");
          setVerifyError(r.detail ? `${human}（${r.detail}）` : human);
        }
      })
      .catch(() => {
        setVerifyPending(false);
        setVerifyError(t("s6.failGeneric"));
      });
  }, [creds, state.configPathId, t]);

  // ── 底部操作条（按状态出按钮；主右危险左）──
  const bar: BarAction[] = useMemo(() => {
    switch (state.status) {
      case "IDLE":
        return [
          {
            labelKey: "btn.startInstall",
            primary: true,
            disabled: !selectedId,
            onClick: startInstall,
          },
        ];
      case "SCAN":
        return [{ labelKey: "btn.scanning", disabled: true, onClick: () => undefined }];
      case "PLAN":
        return [
          { labelKey: "btn.back", onClick: () => dispatch({ type: "BACK" }) },
          { labelKey: "btn.confirm", primary: true, onClick: () => dispatch({ type: "CONFIRM" }) },
        ];
      case "CONFIRM":
        return [
          { labelKey: "btn.back", onClick: () => dispatch({ type: "BACK" }) },
          { labelKey: "btn.begin", primary: true, onClick: () => dispatch({ type: "BEGIN" }) },
        ];
      case "EXECUTE":
        return [
          {
            labelKey: "btn.cancel",
            danger: true,
            onClick: () =>
              dispatch({ type: "ERROR", code: "USER_CANCELLED", messageKey: "err.cancelled" }),
          },
          {
            labelKey: "btn.viewLog",
            onClick: () => setOverlay("settings"),
          },
        ];
      case "CONFIG":
        return [{ labelKey: "btn.skipConfig", onClick: () => dispatch({ type: "CONFIG_WRITTEN", pathId: "none" }) }];
      case "VERIFY":
        return [
          { labelKey: "btn.reVerify", onClick: sendVerify },
          { labelKey: "btn.verify", primary: true, disabled: verifyPending, onClick: sendVerify },
        ];
      case "DONE":
        return [
          { labelKey: "btn.again", onClick: () => dispatch({ type: "RESTART" }) },
          { labelKey: "btn.docs", primary: true, onClick: () => window.open("https://aibox.im/docs", "_blank") },
        ];
      case "ERROR":
        return [];
      default:
        return [];
    }
  }, [state.status, selectedId, startInstall, sendVerify, verifyPending]);

  const screen = screenFor(state.status);
  const isOverlay = overlay !== "none";

  return (
    <>
      <AppShell
        status={state.status}
        errorFrom={state.error?.from}
        bar={isOverlay ? [] : bar}
        onOpenSettings={() => setOverlay("settings")}
        onOpenSmartScreen={() => setOverlay("smartscreen")}
        banner={
          banner.visible ? (
            <div className="flex items-center justify-between bg-[#f5c518] px-5 py-2 text-sm text-aibox-bg">
              <span>
                {t("update.banner", { version: banner.version ?? "?" })}
              </span>
              <div className="flex gap-3">
                <button
                  type="button"
                  className="font-semibold underline"
                  onClick={() => window.open(banner.url, "_blank")}
                >
                  {t("update.download")}
                </button>
                <button type="button" className="font-mono text-xs" onClick={banner.dismiss}>
                  {t("update.later")}
                </button>
              </div>
            </div>
          ) : toast ? (
            <div className="bg-aibox-info/15 px-5 py-2 font-mono text-xs text-aibox-info">{toast}</div>
          ) : undefined
        }
      >
        {isOverlay ? (
          overlay === "settings" ? (
            <SettingsNetwork onClose={() => setOverlay("none")} />
          ) : (
            <SmartScreen onClose={() => setOverlay("none")} />
          )
        ) : screen === "S1Idle" ? (
          <S1Idle
            index={appIndex}
            scan={state.scan}
            loading={indexLoading}
            selectedId={selectedId}
            onSelect={setSelectedId}
            onOpenDoctor={() => setOverlay("settings")}
          />
        ) : screen === "S2Scan" ? (
          <S2Scan report={state.scan} plan={state.plan} />
        ) : screen === "S3Plan" ? (
          <S3Plan plan={state.plan} status={state.status} />
        ) : screen === "S4Execute" ? (
          <S4Execute state={state} />
        ) : screen === "S5Config" ? (
          <S5Config
            appId={state.appId}
            selectedPathId={state.configPathId}
            onSelectPath={(id) => dispatch({ type: "CONFIG_SELECT", pathId: id })}
            onWritten={(id, c) => {
              setCreds(c);
              dispatch({ type: "CONFIG_WRITTEN", pathId: id });
            }}
          />
        ) : screen === "S6Verify" ? (
          <S6Verify
            result={verifyResult?.text ?? null}
            pending={verifyPending}
            latencyMs={verifyResult?.ms ?? null}
            error={verifyError}
            onRetry={sendVerify}
          />
        ) : (
          <S7Done state={state} onRestart={() => dispatch({ type: "RESTART" })} />
        )}

        {/* 断点续装询问弹层 */}
        {resume.asking && (
          <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/70 px-4">
            <div className="w-full max-w-sm rounded-[6px] border border-aibox-green bg-aibox-raised p-5">
              <h2 className="mb-2 font-display text-base font-semibold text-aibox-cream">
                {t("resume.title")}
              </h2>
              <p className="mb-4 text-sm text-aibox-text2">{t("resume.desc")}</p>
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  onClick={resume.dismiss}
                  className="rounded-[4px] border border-aibox-line px-4 py-2 font-mono text-xs text-aibox-text2"
                >
                  {t("resume.no")}
                </button>
                <button
                  type="button"
                  onClick={resume.resume}
                  className="rounded-[4px] bg-aibox-green px-4 py-2 font-display text-sm font-semibold text-aibox-bg"
                >
                  {t("resume.yes")}
                </button>
              </div>
            </div>
          </div>
        )}

        {/* ERROR 覆盖层（任意态进入；E1 面板打开时让位，避免双层遮罩） */}
        {state.status === "ERROR" && !e1.open && (
          <ErrorOverlay
            state={state}
            onRetry={() => dispatch({ type: "RETRY" })}
            onSkip={() => dispatch({ type: "SKIP" })}
            onExported={() => {
              exportedRef.current = true;
            }}
            onDismiss={() => {
              dispatch({ type: exportedRef.current ? "EXPORT_DIAG" : "RETRY" });
              exportedRef.current = false;
            }}
          />
        )}

        {/* E1 内嵌诊断面板（同一任务连败 2 次 → 从右滑出；关闭/重试回到 S4） */}
        <E1DiagPanel
          open={e1.open}
          onClose={() => {
            rawE1({ type: "ACK" });
            dispatch({ type: "RETRY" });
          }}
          onRetry={() => {
            rawE1({ type: "ACK" });
            dispatch({ type: "RETRY" });
          }}
        />
      </AppShell>

      {/* 语言切换（右下角常驻） */}
      <button
        type="button"
        onClick={() => {
          const isZh = (i18next.resolvedLanguage ?? "en").startsWith("zh");
          setLang((isZh ? "en" : "zh-CN") as Lang);
        }}
        className="fixed bottom-14 right-3 z-30 rounded-[4px] border border-aibox-line bg-aibox-raised px-2.5 py-1 font-mono text-[11px] text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        aria-label="switch language"
      >
        {t("app.langSwitch")}
      </button>

      {/* 版本角标（信任细节：关于页可扩展） */}
      <span className="fixed bottom-14 left-20 z-30 font-mono text-[10px] text-aibox-text2/60">
        v{version}
      </span>
    </>
  );
}
