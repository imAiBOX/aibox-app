import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { openExternal } from "./lib/openExternal";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { setLang, SUPPORTED_LANGS, type Lang } from "./i18n";
import { hotLoadInto } from "./i18n/loader";
import i18n from "./i18n";
import {
  createInitialState,
  createPersistence,
  e1Reduce,
  createE1State,
  reduce,
  screenFor,
  buildPlan,
  startExecute,
  type ExecuteHandle,
} from "./state";
import { api, getMockScenario, isMock, type AppIndex, type NetRegionResult } from "./net/client";
import { staticRegionFallback } from "./net/region";
import { channelToast, feedLogFromEvents, feedToastFromEvents, onEvent, startTauriEvents } from "./net/events";
import { AppShell, type BarAction } from "./screens/Shell";
import S1Idle from "./screens/S1Idle";
import S2Scan from "./screens/S2Scan";
import S3Plan from "./screens/S3Plan";
import S4Execute from "./screens/S4Execute";
import S5Config from "./screens/S5Config";
import S6Verify from "./screens/S6Verify";
import S7Done from "./screens/S7Done";
import ErrorOverlay from "./screens/ErrorOverlay";
import Settings from "./screens/Settings";
import S8Manage from "./screens/S8Manage";
import SmartScreen from "./screens/SmartScreen";
import { useUpdateBanner, OFFICIAL_DOWNLOAD_URL } from "./hooks/useUpdateBanner";
import { useUpdateChecks } from "./hooks/useUpdateChecks";
import { buildTrayLabels } from "./state/tray";
import { APP_UPDATE_TTL_MS } from "./state/appUpdates";
import { useSessionResume } from "./hooks/useSessionResume";
import E1DiagPanel from "./screens/E1DiagPanel";
import { runVerify } from "./config/flow";
import type { ConfigCreds } from "./screens/S5Config";

type Overlay = "none" | "settings" | "smartscreen" | "manage";

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
  /** 托盘「退出」在安装进行中 → 确认弹层（防误杀） */
  const [quitConfirm, setQuitConfirm] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  /** 区域判定结果（plan.netRegion + E1 建议分支用） */
  const [netRegion, setNetRegion] = useState<NetRegionResult | null>(null);
  /** 「换个下载源」：一次性跳过通道（取后清空）；当前下载通道（换源重发用） */
  const skipChannelsRef = useRef<string[]>([]);
  const lastChannelRef = useRef<string | null>(null);
  // 断点续装：真 Tauri → SQLite（sessions/kv），mock/浏览器 → memory（persistence-sqlite 工厂）
  const persistence = useRef(createPersistence()).current;
  /** 本次安装的 registry 版本（install-manifest 登记用）；记录去重 */
  const installVersionRef = useRef<string | undefined>(undefined);
  const recordedRef = useRef<string | null>(null);
  // E1 内嵌诊断面板（同一任务连败 2 次触发；纯 reducer 可测）
  const [e1, rawE1] = useReducer(e1Reduce, createE1State());
  /** 导出成功后关闭 → 用 EXPORT_DIAG 语义恢复（与 RETRY 同转移，保留事件契约） */
  const exportedRef = useRef(false);
  const banner = useUpdateBanner();
  const checks = useUpdateChecks();
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
  // 任务成功 → 连败链断裂（同时清掉上一任务的下载通道记录）
  useEffect(() => {
    if (state.status !== "EXECUTE") return;
    const cur = state.tasks[state.currentTaskIndex];
    if (cur?.status === "done") {
      rawE1({ type: "SUCCESS", taskId: cur.id });
      lastChannelRef.current = null;
    }
  }, [state.status, state.tasks, state.currentTaskIndex]);

  // 进入 EXECUTE：重置下载通道记录（换源按钮只针对当前任务）
  useEffect(() => {
    if (state.status === "EXECUTE") lastChannelRef.current = null;
  }, [state.status]);

  // 记录当前下载通道（download_progress / channel_switched → 换源按钮 skipChannels）
  useEffect(
    () =>
      onEvent((e) => {
        if (e.event === "download_progress") lastChannelRef.current = e.payload.channel;
        else if (e.event === "channel_switched") lastChannelRef.current = e.payload.to;
      }),
    [],
  );

  // 目录新增比对（registry_index 就绪后；首启无记录 → 只落记录不提示）
  useEffect(() => {
    if (!appIndex) return;
    void checks.checkNewApps(appIndex.apps.map((a) => a.id));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [appIndex]);

  const { recheck: recheckAppUpdates } = checks;
  // 常驻周期检查（每 4h，窗口隐藏也跑）：已装应用（4h kv 门控）+ 本体（5min 缓存）+ 目录（12h 缓存）
  useEffect(() => {
    const id = setInterval(() => {
      void recheckAppUpdates();
      banner.recheck();
      api.registryIndex()
        .then((idx) => setAppIndex(idx))
        .catch(() => undefined);
    }, APP_UPDATE_TTL_MS);
    return () => clearInterval(id);
  }, [recheckAppUpdates, banner.recheck]);

  // 托盘状态同步（i18n 后下发 tooltip/菜单；本体新版/可升级数量联动）
  const appUpdateCount = checks.appUpdates.filter((r) => r.hasUpdate).length;
  useEffect(() => {
    const labels = buildTrayLabels(t, {
      hasCoreUpdate: banner.info?.hasUpdate ?? false,
      coreVersion: banner.info?.version,
      appUpdateCount,
    });
    void api.traySync(labels).catch(() => undefined);
  }, [t, i18next.resolvedLanguage, banner.info, appUpdateCount]);

  // 托盘菜单动作（Rust 发出）：下载=开官网；检查=三类合并重查；退出=安装中先确认
  useEffect(
    () =>
      onEvent((e) => {
        if (e.event !== "tray_action") return;
        if (e.payload.action === "download") {
          void openExternal(OFFICIAL_DOWNLOAD_URL);
        } else if (e.payload.action === "check") {
          void recheckAppUpdates({ force: true });
          banner.recheck();
          api.registryIndex()
            .then((idx) => setAppIndex(idx))
            .catch(() => undefined);
        } else if (e.payload.action === "quit") {
          if (state.status === "EXECUTE") setQuitConfirm(true);
          else void api.appQuit().catch(() => undefined);
        }
      }),
    [recheckAppUpdates, banner.recheck, state.status],
  );

  // 安装完成（任务全部干完 → CONFIG）→ 登记 install-manifest（已装应用更新检查的数据源）
  useEffect(() => {
    const appId = state.appId;
    if (state.status !== "CONFIG" || !appId) return;
    if (recordedRef.current === appId) return;
    recordedRef.current = appId;
    void (async () => {
      const version =
        installVersionRef.current ??
        (await api
          .registryApp(appId)
          .then((m) => m.version)
          .catch(() => undefined));
      await api.installRecord(appId, version).catch(() => undefined);
    })();
  }, [state.status, state.appId]);

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

  // ── 主流程编排（mock 数据驱动状态机；换真 invoke；真四阶段接线）──
  const startInstall = useCallback(() => {
    if (!selectedId) return;
    dispatch({ type: "START_INSTALL" });
    // scan 与 manifest 并行拉取；PLAN 构建需两者（platform/arch 差集 + 路线），Promise.all 保证新鲜
    const scanP = api.scanSystem().then((report) => {
      dispatch({ type: "SCAN_DONE", report });
      return report;
    });
    scanP.catch((_e: unknown) =>
      dispatch({ type: "ERROR", code: "SCAN_FAILED", messageKey: "err.scanFailed" }),
    );
    void Promise.all([scanP, api.registryApp(selectedId)])
      .then(async ([report, manifest]) => {
        installVersionRef.current = manifest.version;
        // 路线按 net.effective 首条可行；region 取不到 → 前端静态判定兑底（不再硬编 global）
        const region = await api.netRegion().catch((e: unknown) => {
          const effective = staticRegionFallback();
          console.warn(
            `[aibox] net_region failed, using static fallback (${effective}):`,
            e,
          );
          return { source: "auto", effective } as NetRegionResult;
        });
        setNetRegion(region);
        // exe 需绝对路径（Rust exec 只展开 cwd 的 ~）；非 Tauri → '~' 兑底
        const home = await (async () => {
          if (isMock()) return "~";
          try {
            const { homeDir } = await import("@tauri-apps/api/path");
            return await homeDir();
          } catch {
            return "~";
          }
        })();
        const plan = buildPlan(manifest, {
          platform: report.os.platform,
          arch: report.os.arch,
          region: region.effective,
          home,
          scan: report,
          netRegion: { source: region.source, effective: region.effective },
        });
        dispatch({ type: "PLAN_READY", plan });
        // 诊断包收录最后计划：写 ~/.aibox/cache/last-plan.json，失败不阻塞
        void api.savePlan(plan).catch(() => undefined);
      })
      .catch(() => dispatch({ type: "ERROR", code: "REGISTRY_FETCH", messageKey: "err.registry" }));
  }, [selectedId]);

  // EXECUTE：真四阶段编排（mock 回落 = 阶段定时器+场景注入，行为不变）
  const execHandle = useRef<ExecuteHandle | null>(null);
  useEffect(() => {
    if (state.status !== "EXECUTE") return;
    const handle = startExecute(
      {
        mode: isMock() ? "mock" : "real",
        api,
        dispatch,
        onEvent,
        sleep: (ms) => new Promise((r) => setTimeout(r, ms)),
        scenario: getMockScenario,
        saveSnapshot: (snap) => persistence.saveSnapshot(snap),
        now: () => Date.now(),
        // 「换个下载源」：一次性消费跳过通道（取后清空，下次重试不再跳）
        takeSkipChannels: () => {
          const skip = skipChannelsRef.current;
          skipChannelsRef.current = [];
          return skip;
        },
      },
      { appId: state.appId, tasks: state.tasks },
    );
    execHandle.current = handle;
    void handle.promise;
    return () => {
      handle.stop();
      execHandle.current = null;
    };
    // 单次进入 EXECUTE 跑一轮；RETRY/SKIP 会经 ERROR 中转重进
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [state.status]);

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
          { labelKey: "btn.docs", primary: true, onClick: () => void openExternal("https://aibox.im/docs") },
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
        onOpenManage={() => setOverlay("manage")}
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
                  onClick={() => void openExternal(banner.url)}
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
            <Settings onClose={() => setOverlay("none")} />
          ) : overlay === "manage" ? (
            <S8Manage
              onClose={() => setOverlay("none")}
              onGotoInstall={(id) => {
                setOverlay("none");
                setSelectedId(id);
                dispatch({ type: "RESTART" });
              }}
            />
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
            appUpdates={checks.appUpdates}
            newApps={checks.newApps}
            onDismissNewApps={checks.dismissNewApps}
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

        {/* 退出确认弹层（托盘「退出」在安装进行中触发；人话防误杀） */}
        {quitConfirm && (
          <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 px-4">
            <div className="w-full max-w-sm rounded-[6px] border border-aibox-danger bg-aibox-raised p-5">
              <h2 className="mb-2 font-display text-base font-semibold text-aibox-cream">
                {t("quit.title")}
              </h2>
              <p className="mb-4 text-sm text-aibox-text2">{t("quit.desc")}</p>
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  onClick={() => setQuitConfirm(false)}
                  className="rounded-[4px] border border-aibox-line px-4 py-2 font-mono text-xs text-aibox-text2"
                >
                  {t("quit.stay")}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setQuitConfirm(false);
                    void api.appQuit().catch(() => undefined);
                  }}
                  className="rounded-[4px] bg-aibox-danger px-4 py-2 font-display text-sm font-semibold text-aibox-bg"
                >
                  {t("quit.quit")}
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
            onRetrySource={() => {
              // 换源语义：跳过当前失败通道重发 download_start
              const ch = lastChannelRef.current;
              skipChannelsRef.current = ch ? [ch] : [];
              dispatch({ type: "RETRY" });
            }}
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
          onRetrySource={() => {
            const ch = lastChannelRef.current;
            skipChannelsRef.current = ch ? [ch] : [];
            rawE1({ type: "ACK" });
            dispatch({ type: "RETRY" });
          }}
          errorCode={state.error?.code}
          regionEffective={netRegion?.effective ?? null}
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

      {/* 语言切换（右下角常驻）：en → zh-CN → zh-TW → en 循环，按钮显示下一个语言 */}
      <button
        type="button"
        onClick={() => {
          const cur = (i18next.resolvedLanguage ?? "en") as Lang;
          const idx = SUPPORTED_LANGS.indexOf(cur);
          setLang(SUPPORTED_LANGS[(idx + 1) % SUPPORTED_LANGS.length]);
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
