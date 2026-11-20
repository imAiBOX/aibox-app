import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { openExternal } from "./lib/openExternal";
import { isMacPlatform } from "./platform";
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
  errorCodeOf,
  messageKeyForCode,
  type ExecuteHandle,
} from "./state";
import {
  api,
  getMockScenario,
  isMock,
  type AppIndex,
  type AppManifest,
  type NetRegionResult,
} from "./net/client";
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
import { usePrefetch } from "./hooks/usePrefetch";
import { buildTrayLabels } from "./state/tray";
import { logDrawer } from "./state/logDrawer";
import { siteUrl } from "./lib/links";
import { hasSeenFirstRun, markFirstRunSeen } from "./state/firstRun";
import { notifyDesktop, trayLocationHint } from "./lib/notify";
import {
  DEFAULT_NOTIFY_PREFS,
  createNotifyStore,
  isQuietNow,
  type NotifyPrefs,
  type UpgradeBackupRecord,
} from "./state/notifyPrefs";
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
  /** 打开设置时是否自动跑一次体检（首屏侧栏「打开体检」触发） */
  const [doctorAutoRun, setDoctorAutoRun] = useState(false);
  /** 管理页聚焦模式（托盘待办「N 个应用可升级」点进来 → 把可升级的排到最前） */
  const [manageFocus, setManageFocus] = useState<"upgrades" | null>(null);
  /** 最近一次「升级前备份」记录（管理页展示，用户可自行恢复） */
  const [upgradeBackup, setUpgradeBackup] = useState<UpgradeBackupRecord | null>(null);
  /** 首启引导是否显示（只显示一次；用户主动关闭后不再打扰） */
  const [showFirstRun, setShowFirstRun] = useState(() => !hasSeenFirstRun());
  /** startInstall 的最新引用（托盘动作在声明之前 → 用 ref 避免 TDZ） */
  const startInstallRef = useRef<((appId?: string) => void) | null>(null);
  /** 通知偏好（用户说了算；持久化） */
  const [notifyPrefs, setNotifyPrefs] = useState<NotifyPrefs>(DEFAULT_NOTIFY_PREFS);
  /** 偏好是否已从存储读出（未读出前不按偏好做「启动即收进托盘」等动作） */
  const [notifyPrefsLoaded, setNotifyPrefsLoaded] = useState(false);
  const notifyStoreRef = useRef(createNotifyStore());
  const [version, setVersion] = useState(__APP_VERSION__);
  const [appIndex, setAppIndex] = useState<AppIndex | null>(null);
  const [indexLoading, setIndexLoading] = useState(true);
  /** registry 目录加载失败码（非空 → S1 显示人话原因 + 重试按钮；不吞错） */
  const [indexError, setIndexError] = useState<string | null>(null);
  /** 目录重载计数（S1「重试」自增 → 重新拉 registry_index） */
  const [indexRetry, setIndexRetry] = useState(0);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  /** 当前应用的 registry manifest（完成页要显示「装到哪了」→ maintenance.dataPaths） */
  const [manifest, setManifest] = useState<AppManifest | null>(null);
  /** C5 写入成功后的凭据（内存态，仅供 VERIFY；不进持久化） */
  const [creds, setCreds] = useState<ConfigCreds | null>(null);
  const [verifyResult, setVerifyResult] = useState<{ text: string; ms: number } | null>(null);
  const [verifyError, setVerifyError] = useState<string | null>(null);
  /** 验证失败的原始报文（折叠进「技术细节」，不直接糊在中文提示后面） */
  const [verifyDetail, setVerifyDetail] = useState<string | null>(null);
  /** 验证失败的机器码（小字，便于用户反馈时给出准确定位） */
  const [verifyCode, setVerifyCode] = useState<string | null>(null);
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
  const banner = useUpdateBanner();
  const checks = useUpdateChecks();
  // 空闲预下载：已装应用有新版 + 常驻 IDLE → 后台静默预下主安装包（升级秒装闭环）
  usePrefetch(checks.appUpdates, state.status);
  /**
   * 断点续装前的准备：重建 install plan（冷启动时 state.plan 为 null → 没有任务队列）。
   *
   * 旧版直接 RESUME_SESSION → 空队列 → 界面永远停在「安装中 / 全部任务完成」，
   * 用户既不能前进也不能退出（死胡同）。现在：先拉 manifest + scan + buildPlan，
   * 注入 RESUME_PLAN 后再 RESUME_SESSION；失败就留在 IDLE。
   */
  const prepareResume = useCallback(
    async (appId: string): Promise<boolean> => {
      try {
        const [report, manifest] = await Promise.all([api.scanSystem(), api.registryApp(appId)]);
        const region = await api.netRegion().catch((e: unknown) => {
          const effective = staticRegionFallback();
          console.warn(`[aibox] net_region failed, using static fallback (${effective}):`, e);
          return { source: "auto", effective } as NetRegionResult;
        });
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
        dispatch({ type: "RESUME_PLAN", plan });
        return true;
      } catch (e) {
        console.warn("[aibox] resume plan rebuild failed:", e);
        return false;
      }
    },
    [dispatch],
  );

  const resume = useSessionResume(persistence, dispatch, state, prepareResume);

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
  // 失败不吞错：记下错误码 → S1 说人话 + 给「重试」按钮（旧版只留一句「检查网络」）
  useEffect(() => {
    let alive = true;
    setIndexLoading(true);
    api
      .registryIndex()
      .then((idx) => {
        if (!alive) return;
        setAppIndex(idx);
        setIndexError(null);
      })
      .catch((e: unknown) => {
        if (!alive) return;
        setAppIndex({ apps: [] });
        setIndexError(errorCodeOf(e, "REGISTRY_UNAVAILABLE"));
      })
      .finally(() => {
        if (alive) setIndexLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [indexRetry]);

  // 真事件桥（download_progress/task_phase/channel_switched/log_line → 统一事件总线；mock 已内建推送）
  useEffect(() => {
    const stopBridge = startTauriEvents();
    // 换源 toast 文案：按 Rust 带上的原因选人话（线路慢 / 线路不可用），文案在 i18n 字典里
    const offToast = feedToastFromEvents(channelToast);
    const offLog = feedLogFromEvents();
    return () => {
      offToast();
      offLog();
      stopBridge();
    };
  }, []);

  // 通道切换 toast：3s 自动消失（不打断流程）
  useEffect(() => channelToast.subscribe(setToast), []);

  // 点 X 关闭 → Rust 侧隐藏到托盘（不退出）。小白看到窗口「啪」地消失、
  // 任务栏也没有它，会直接判定「软件崩了」→ 给一次明确提示（托盘在哪、怎么找回）。
  useEffect(() => {
    if (isMock()) return;
    let unlisten: (() => void) | undefined;
    let alive = true;
    void (async () => {
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        const win = getCurrentWindow();
        const off = await win.onCloseRequested((event) => {
          // 自己处理：先提示（**按平台说清在哪**：Windows=右下角任务栏，macOS=右上角菜单栏），再隐藏
          if (alive) {
            channelToast.show(
              t("tray.closedHint", { where: trayLocationHint(i18next.resolvedLanguage) }),
            );
          }
          event.preventDefault();
          void win.hide();
        });
        if (!alive) off();
        else unlisten = off;
      } catch {
        /* 非 Tauri / 旧版本 → 落回 Rust 侧默认行为（隐藏，不提示） */
      }
    })();
    return () => {
      alive = false;
      unlisten?.();
    };
  }, [t, i18next.resolvedLanguage]);

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

  // 载入「最近一次升级前备份」记录（管理页展示）
  useEffect(() => {
    void notifyStoreRef.current.loadUpgradeBackup().then(setUpgradeBackup);
  }, []);

  // 载入通知偏好（一次性）
  useEffect(() => {
    void notifyStoreRef.current.loadPrefs().then((p) => {
      setNotifyPrefs(p);
      setNotifyPrefsLoaded(true);
    });
  }, []);

  /**
   * **启动即收进托盘**（用户要的「运行后最小化到右下角」）。
   *
   * 只在本次启动生效一次（ref 守卫）：用户之后在设置里打开这个开关时，
   * 不能把正在用的窗口也收走（那会让人以为程序崩了）。
   */
  const startupMinRef = useRef(false);
  useEffect(() => {
    if (!notifyPrefsLoaded) return;
    if (startupMinRef.current) return;
    if (!notifyPrefs.startMinimized) return;
    startupMinRef.current = true;
    void (async () => {
      try {
        const { getCurrentWindow } = await import("@tauri-apps/api/window");
        await getCurrentWindow().hide();
      } catch {
        /* 非 Tauri → 不处理 */
      }
      // **启动就收进托盘 = 双击后什么都看不到** → 必须告诉用户「我在哪」，
      // 否则用户会以为程序没启动（同一台机器只提醒一次）。
      const first = await notifyStoreRef.current.markSeen("first-minimized");
      if (first) {
        await notifyDesktop(
          t("notify.startedHiddenTitle"),
          t("notify.startedHiddenBody", {
            where: trayLocationHint(i18next.resolvedLanguage),
          }),
        );
      }
    })();
  }, [notifyPrefsLoaded, notifyPrefs.startMinimized, t, i18next.resolvedLanguage]);

  /**
   * **装完 10 秒后收进托盘**（可被任何操作取消）。
   *
   * 为什么不是立刻收：完成页是整个流程里最有价值的时刻（「装到哪了」「怎么打开」），
   * 一装完就消失等于白做。为什么可以延后：用户点开应用、看完完成页，
   * 一次交互就该取消自动收（否则会「我正看着它突然没了」）。
   */
  const afterInstallRef = useRef(false);
  useEffect(() => {
    if (state.status !== "DONE") {
      afterInstallRef.current = false;
      return;
    }
    if (!notifyPrefsLoaded || !notifyPrefs.minimizeAfterInstall) return;
    if (afterInstallRef.current) return;
    afterInstallRef.current = true;
    const cancel = () => {
      afterInstallRef.current = false;
      window.removeEventListener("pointerdown", cancel);
      window.removeEventListener("keydown", cancel);
    };
    const id = window.setTimeout(() => {
      cancel();
      void (async () => {
        try {
          const { getCurrentWindow } = await import("@tauri-apps/api/window");
          await getCurrentWindow().hide();
        } catch {
          /* 非 Tauri */
        }
      })();
    }, 10_000);
    window.addEventListener("pointerdown", cancel);
    window.addEventListener("keydown", cancel);
    return () => {
      window.clearTimeout(id);
      window.removeEventListener("pointerdown", cancel);
      window.removeEventListener("keydown", cancel);
    };
  }, [state.status, notifyPrefs.minimizeAfterInstall, notifyPrefsLoaded]);

  /**
   * 最近安装的应用（托盘右键直接装，不用进主界面）。
   *
   * 数据源 = install-manifest（AiBOX 自己装的那些，按安装顺序）；
   * **诚实命名**：这里只知道「安装先后」，不知道「使用频次」→ 文案写「最近安装的」。
   */
  const [recentApps, setRecentApps] = useState<{ id: string; name: string }[]>([]);
  useEffect(() => {
    let alive = true;
    void (async () => {
      try {
        const list = await api.manageManifestList();
        if (!alive) return;
        const ids = list.map((e) => e.appId).slice(-3).reverse();
        if (ids.length === 0) return;
        const names = await Promise.all(
          ids.map(async (id) => {
            try {
              const m = await api.registryApp(id);
              return { id, name: m.name ?? id };
            } catch {
              return { id, name: id };
            }
          }),
        );
        if (alive) setRecentApps(names);
      } catch {
        /* 拿不到就不显示（不硬凑） */
      }
    })();
    return () => {
      alive = false;
    };
  }, []);

  // 托盘状态同步（i18n 后下发 tooltip/菜单；本体新版/可升级数量联动）
  const appUpdateCount = checks.appUpdates.filter((r) => r.hasUpdate).length;
  useEffect(() => {
    const labels = buildTrayLabels(t, {
      hasCoreUpdate: banner.info?.hasUpdate ?? false,
      coreVersion: banner.info?.version,
      appUpdateCount,
      newAppCount: checks.newApps.length,
      recent: recentApps,
    });
    void api.traySync(labels).catch(() => undefined);
  }, [
    t,
    i18next.resolvedLanguage,
    banner.info,
    appUpdateCount,
    checks.newApps.length,
    recentApps,
  ]);

  /**
   * 系统通知：**窗口关着时唯一能让用户感知的通道**（AiBOX 常驻托盘 → 界面黄条用户看不到）。
   *
   * 克制三原则：
   * ① 偏好关掉的类别一律不发；② 同一事件（同一应用同一版本）只发一次（markSeen 去重）；
   * ③ 正文写人话，不写错误码/内部词。
   */
  useEffect(() => {
    if (!notifyPrefsLoaded) return;
    const pending = checks.appUpdates.filter((r) => r.hasUpdate && r.latest);
    if (pending.length === 0) return;
    if (!notifyPrefs.appUpdate) return;
    // 免打扰时段 → 不发、也**不标记**（标记了就永远补不上了）
    if (isQuietNow(notifyPrefs)) return;
    void (async () => {
      const fresh = [];
      for (const r of pending) {
        if (await notifyStoreRef.current.markSeen(`upd:${r.appId}@${r.latest}`)) fresh.push(r);
      }
      if (fresh.length === 0) return;
      await notifyDesktop(
        t("notify.appUpdateTitle", { count: fresh.length }),
        fresh.length === 1
          ? t("notify.appUpdateBodyOne", { name: fresh[0].appId, version: fresh[0].latest })
          : t("notify.appUpdateBodyMany", { count: fresh.length }),
      );
    })();
  }, [checks.appUpdates, notifyPrefs.appUpdate, notifyPrefsLoaded, t]);

  // 本体有新版 → 通知（每版只发一次）
  useEffect(() => {
    if (!notifyPrefsLoaded || !notifyPrefs.coreUpdate) return;
    if (isQuietNow(notifyPrefs)) return;
    const info = banner.info;
    if (!info?.hasUpdate || !info.version) return;
    void (async () => {
      const key = `core:${info.version}`;
      if (!(await notifyStoreRef.current.markSeen(key))) return;
      await notifyDesktop(
        t("notify.coreUpdateTitle"),
        t("notify.coreUpdateBody", { version: info.version }),
      );
    })();
  }, [banner.info, notifyPrefs.coreUpdate, notifyPrefsLoaded, t]);

  // 目录上新 → 通知（每个新应用只提醒一次）
  useEffect(() => {
    if (!notifyPrefsLoaded || !notifyPrefs.newApps) return;
    if (isQuietNow(notifyPrefs)) return;
    if (checks.newApps.length === 0) return;
    void (async () => {
      const fresh: string[] = [];
      for (const id of checks.newApps) {
        if (await notifyStoreRef.current.markSeen(`new:${id}`)) fresh.push(id);
      }
      if (fresh.length === 0) return;
      await notifyDesktop(
        t("notify.newAppsTitle"),
        t("notify.newAppsBody", { count: fresh.length }),
      );
    })();
  }, [checks.newApps, notifyPrefs.newApps, notifyPrefsLoaded, t]);

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
        } else if (e.payload.action === "updates") {
          // 托盘待办「N 个应用可升级」→ 打开窗口并直接进「管理已装」（且聚焦到可升级项）
          setManageFocus("upgrades");
          setOverlay("manage");
        } else if (e.payload.action === "newapps") {
          // 托盘待办「N 个新应用」→ 打开窗口并回到应用列表（S1）
          // 注意：RESTART 只在 DONE 态有效；其他态只开窗口，不打断正在进行的流程。
          setOverlay("none");
          if (state.status === "DONE") dispatch({ type: "RESTART" });
          else if (state.status === "IDLE") setAppIndex((idx) => idx);
        } else if (e.payload.action.startsWith("install:")) {
          // 托盘「最近安装的 → 安装 X」→ 直接开始装那个应用（不进主界面）
          const appId = e.payload.action.slice("install:".length);
          if (appId) void startInstallRef.current?.(appId);
        } else if (e.payload.action === "core") {
          // 托盘待办「AiBOX 有新版」→ 打开窗口 + 跳更新条
          void openExternal(siteUrl("/download/", i18next.resolvedLanguage));
        } else if (e.payload.action === "quit") {
          if (state.status === "EXECUTE") setQuitConfirm(true);
          else void api.appQuit().catch(() => undefined);
        }
      }),
    [recheckAppUpdates, banner.recheck, state.status, i18next.resolvedLanguage],
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
  /**
   * 升级链路的**前置动作**：备份该应用的数据目录（safety net）。
   *
   * 为什么：升级会重跑安装程序，可能覆盖/重置应用配置（key、账号、偏好）。
   * 卸载路径本来就有备份（`manage_backup`），**升级路径却一直没有** ——
   * 升级前先备份一份，坏了还能回去。备份失败**不阻断升级**（只提示）。
   */
  const backupBeforeUpgrade = useCallback(
    async (appId: string): Promise<string | null> => {
      try {
        const manifest = await api.registryApp(appId);
        const dataPaths = manifest.maintenance?.dataPaths ?? [];
        if (dataPaths.length === 0) return null;
        const out = await api.manageBackup(appId, dataPaths);
        return out.backupDir || null;
      } catch {
        return null;
      }
    },
    [],
  );

  /** 升级队列（批量升级：一次排队多个，装完一个自动接下一个） */
  const upgradeQueueRef = useRef<string[]>([]);

  const startInstall = useCallback((appIdOverride?: string) => {
    const appId = appIdOverride ?? selectedId;
    if (!appId) return;
    dispatch({ type: "START_INSTALL" });
    // scan 与 manifest 并行拉取；PLAN 构建需两者（platform/arch 差集 + 路线），Promise.all 保证新鲜
    const scanP = api.scanSystem().then((report) => {
      dispatch({ type: "SCAN_DONE", report });
      return report;
    });
    scanP.catch((_e: unknown) =>
      dispatch({ type: "ERROR", code: "SCAN_FAILED", messageKey: "err.scanFailed" }),
    );
    void Promise.all([scanP, api.registryApp(appId)])
      .then(async ([report, manifest]) => {
        installVersionRef.current = manifest.version;
        setManifest(manifest);
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
      .catch((e: unknown) => {
        // 错误码区分「网络不通」与「目录源对不上/校验失败」（后者让用户查网络是误导）
        const code = errorCodeOf(e, "REGISTRY_FETCH");
        dispatch({ type: "ERROR", code, messageKey: messageKeyForCode(code) });
      });
  }, [selectedId]);

  /**
   * 跑一次升级（含前置备份 + 升级模式标记）。
   *
   * 批量升级：`queue` 是后面还要升的应用 id；当前这个装完后自动接力下一个
   * （见下面的 effect）。中途「取消」会清空队列（不再继续升）。
   */
  const runUpgrade = useCallback(
    async (appId: string, queue: string[] = []) => {
      upgradeQueueRef.current = queue;
      // 升级前先备份数据目录（safety net；失败不阻断升级）
      const backupDir = await backupBeforeUpgrade(appId);
      if (backupDir) {
        // 记档 → 管理页能查到「上次升级前的备份在哪」（用户可自行恢复）
        await notifyStoreRef.current.saveUpgradeBackup({
          appId,
          path: backupDir,
          at: Date.now(),
        });
        channelToast.show(t("s8.backupDone", { path: backupDir }));
      }
      setSelectedId(appId);
      dispatch({ type: "RESTART" });
      dispatch({ type: "UPGRADE_MODE" });
      startInstall(appId);
    },
    [backupBeforeUpgrade, startInstall, t],
  );


  // EXECUTE：真四阶段编排（mock 回落 = 阶段定时器+场景注入，行为不变）
  // 把最新的 startInstall 放进 ref（托盘动作需要在声明之前也能调到）
  startInstallRef.current = startInstall;

  // 批量升级接力：本次升级完成（DONE）→ 自动开始队列里的下一个
  useEffect(() => {
    if (state.status !== "DONE") return;
    const next = upgradeQueueRef.current.shift();
    if (!next) return;
    void runUpgrade(next);
  }, [state.status, runUpgrade]);

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

  // 诊断包收录最后计划：**执行结束后回写真实任务状态**。
  // 旧行为：last-plan.json 只在计划生成时写一次（status 永远 "pending"），
  // 用户导出诊断包时看不到“装到哪一步、哪个任务失败了”。2026-11-19 起回写。
  useEffect(() => {
    if (!state.plan) return;
    if (state.status === "IDLE" || state.status === "SCAN" || state.status === "PLAN" || state.status === "CONFIRM") {
      return;
    }
    void api.savePlan({ ...state.plan, tasks: state.tasks }).catch(() => undefined);
  }, [state.status]);

  const sendVerify = useCallback(() => {
    setVerifyPending(true);
    setVerifyError(null);
    setVerifyDetail(null);
    setVerifyCode(null);
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
          const human = r.errorKey ? t(r.errorKey) : t("err.verifyFailed");
          setVerifyError(human);
          setVerifyDetail(r.detail ?? null);
          setVerifyCode(r.errorCode ?? null);
        }
      })
      .catch(() => {
        setVerifyPending(false);
        setVerifyError(t("err.verifyFailed"));
        setVerifyDetail(null);
        setVerifyCode(null);
      });
  }, [creds, state.configPathId, t]);

  /** 当前选中的应用名（底栏按钮文案用：「安装 OpenClaw」而不是笼统的「开始安装」） */
  const selectedName = useMemo(
    () => appIndex?.apps.find((a) => a.id === selectedId)?.name ?? null,
    [appIndex, selectedId],
  );

  // ── 底部操作条（按状态出按钮；主右危险左）──
  const bar: BarAction[] = useMemo(() => {
    switch (state.status) {
      case "IDLE":
        return [
          {
            // 动态文案：按钮自己说明白「装的是哪个」（旧版统一叫「开始安装」，
            // 选没选中都看不出区别）
            labelKey: selectedName ? "btn.startInstallNamed" : "btn.startInstall",
            labelArgs: selectedName ? { name: selectedName } : undefined,
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
            onClick: () => {
              // 取消 = **真取消**：停掉在途下载 + 回应用列表（不重跑）。
              // 旧版取消只发一个 ERROR，点「关闭」又会 recover() → EXECUTE → 安装重新跑起来。
              execHandle.current?.stop();
              logDrawer.close();
              // 主动取消 = 放弃这次安装 → **清掉会话快照**，
              // 否则下次启动还会问「继续上次安装？」（用户上次明明主动取消了）
              void persistence.clearSession();
              upgradeQueueRef.current = [];
              channelToast.show(t("btn.cancelDone"));
              dispatch({ type: "DISMISS" });
            },
          },
          {
            labelKey: "btn.viewLog",
            onClick: () => logDrawer.open(),
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
          {
            labelKey: "btn.docs",
            primary: true,
            // 指向当前应用的教程页（原先 /docs 在站点上是 404）
            onClick: () =>
              void openExternal(siteUrl(`/apps/${state.appId ?? "openclaw"}/`, i18next.resolvedLanguage)),
          },
        ];
      case "ERROR":
        return [];
      default:
        return [];
    }
  }, [state.status, selectedId, selectedName, startInstall, sendVerify, verifyPending]);

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
                  onClick={() => {
                    // 直链直下：dl.aibox.im/vX.Y.Z/<platform>（拍板项）；短链不可用回落官网页
                    const ver = banner.version ? String(banner.version).replace(/^v/, "") : "";
                    const platform = isMacPlatform() ? "mac" : "win";
                    const direct = ver ? `https://dl.aibox.im/v${ver}/${platform}` : null;
                    // 回落官网下载页（原先 banner.url 指向根路径 /download/ → 404）
                    const fallback = siteUrl("/download/", i18next.resolvedLanguage);
                    if (!direct) {
                      void openExternal(fallback);
                      return;
                    }
                    fetch(direct, { method: "HEAD", redirect: "follow" })
                      .then((r) => void openExternal(r.ok ? direct : fallback))
                      .catch(() => void openExternal(fallback));
                  }}
                >
                  {t("update.download")}
                </button>
                <button
                  type="button"
                  className="font-mono text-xs"
                  onClick={() => banner.dismiss(banner.version ?? undefined)}
                >
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
            <Settings
            onClose={() => {
              setOverlay("none");
              setDoctorAutoRun(false);
            }}
            autoRunDoctor={doctorAutoRun}
            notifyPrefs={notifyPrefs}
            onNotifyPrefsChange={(p) => {
              setNotifyPrefs(p);
              void notifyStoreRef.current.savePrefs(p);
            }}
            trayHint={trayLocationHint(i18next.resolvedLanguage)}
          />
          ) : overlay === "manage" ? (
            <S8Manage
              focus={manageFocus}
              upgradeBackup={upgradeBackup}
              onClose={() => {
                setOverlay("none");
                setManageFocus(null);
              }}
              onGotoInstall={(id) => {
                setOverlay("none");
                setSelectedId(id);
                // 回到 IDLE 再进安装流程（RESTART 只在 DONE 态生效；其他态本来就是 IDLE）
                dispatch({ type: "RESTART" });
                // **一路到底**：不再把用户丢回应用列表让他自己找、自己再点一次「开始安装」，
                // 直接进入该应用的安装计划确认页（确认后即装）。
                void runUpgrade(id);
              }}
              onUpgradeAll={(ids) => {
                setOverlay("none");
                void runUpgrade(ids[0], ids.slice(1));
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
            indexError={indexError}
            onRetryIndex={() => setIndexRetry((n) => n + 1)}
            installedIds={checks.appUpdates.map((r) => r.appId)}
            selectedId={selectedId}
            onSelect={setSelectedId}
            // 「打开体检」= 打开设置的网络页**并自动跑一次体检**
            // （旧版只打开设置页 → 用户看到的是「地区选择」，与「看看你的电脑」对不上）
            onOpenDoctor={() => {
              setDoctorAutoRun(true);
              setOverlay("settings");
            }}
            appUpdates={checks.appUpdates}
            newApps={checks.newApps}
            onDismissNewApps={checks.dismissNewApps}
          />
        ) : screen === "S2Scan" ? (
          <S2Scan report={state.scan} plan={state.plan} />
        ) : screen === "S3Plan" ? (
          <S3Plan plan={state.plan} status={state.status} scan={state.scan} />
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
            errorCode={verifyCode}
            errorDetail={verifyDetail}
            onRetry={sendVerify}
          />
        ) : (
          <S7Done
            state={state}
            dataPaths={manifest?.maintenance?.dataPaths ?? []}
            onRestart={() => dispatch({ type: "RESTART" })}
          />
        )}

        {/* 首启引导：一句话 + 三条要点 + 一个按钮（不是长教程） */}
        {showFirstRun && (
          <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/70 px-4">
            <div className="w-full max-w-sm rounded-[6px] border border-aibox-green bg-aibox-raised p-5">
              <h2 className="mb-2 font-display text-lg font-semibold text-aibox-cream">
                {t("first.title")}
              </h2>
              <p className="mb-4 text-sm leading-relaxed text-aibox-text2">{t("first.lede")}</p>
              <ul className="mb-5 flex flex-col gap-2">
                {[0, 1, 2].map((i) => (
                  <li key={i} className="flex gap-2 text-xs leading-relaxed text-aibox-text2">
                    <span className="text-aibox-green">√</span>
                    <span>{t(`first.point${i + 1}`)}</span>
                  </li>
                ))}
              </ul>
              <div className="flex items-center justify-between gap-3">
                <button
                  type="button"
                  onClick={() => {
                    markFirstRunSeen();
                    setShowFirstRun(false);
                  }}
                  className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green"
                >
                  {t("first.skip")}
                </button>
                <button
                  type="button"
                  onClick={() => {
                    markFirstRunSeen();
                    setShowFirstRun(false);
                  }}
                  className="rounded-[4px] bg-aibox-green px-4 py-2 font-display text-sm font-semibold text-aibox-bg"
                >
                  {t("first.start")}
                </button>
              </div>
            </div>
          </div>
        )}

        {/* 断点续装询问弹层 */}
        {resume.asking && (
          <div className="fixed inset-0 z-40 flex items-center justify-center bg-black/70 px-4">
            <div className="w-full max-w-sm rounded-[6px] border border-aibox-green bg-aibox-raised p-5">
              <h2 className="mb-2 font-display text-base font-semibold text-aibox-cream">
                {t("resume.title")}
              </h2>
              <p className="mb-4 text-sm text-aibox-text2">{t("resume.desc")}</p>
              {/* 旧版只说「上次安装没走完」，不说是哪个应用、到第几步 → 用户只能赌 */}
              {resume.snapshot && (
                <p className="mb-4 text-xs leading-relaxed text-aibox-text2">
                  {t("resume.which", {
                    name:
                      appIndex?.apps.find((a) => a.id === resume.snapshot?.appId)?.name ??
                      resume.snapshot.appId,
                    done: resume.snapshot.doneTaskIds.length,
                  })}
                </p>
              )}
              <div className="flex justify-end gap-2">
                <button
                  type="button"
                  onClick={() => {
                    // 「重新开始」= 丢弃这次会话（真清快照），否则点了等于没点
                    void persistence.clearSession();
                    resume.dismiss();
                  }}
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
              // 导出完成（副作用已发生）；关闭按钮走 DISMISS，不重跑安装
            }}
            onDismiss={() => {
              // 「关闭」= **放弃这次安装并返回应用列表**。
              // 旧版这里 dispatch RETRY/EXPORT_DIAG → recover() → 回到 EXECUTE →
              // useEffect 重新 startExecute() → 用户点「关闭」反而把安装又跑起来了。
              logDrawer.close();
              // 「不重试，返回」= 放弃 → 同样清掉会话快照与升级队列
              void persistence.clearSession();
              upgradeQueueRef.current = [];
              dispatch({ type: "DISMISS" });
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
          onSkip={() => {
            // 「跳过这个组件」：E1 打开时 ErrorOverlay 是隐藏的 → 这里必须给出口，
            // 否则用户被困在「只能一直重试」的循环里。
            rawE1({ type: "ACK" });
            dispatch({ type: "SKIP" });
          }}
          errorCode={state.error?.code}
          regionEffective={netRegion?.effective ?? null}
          onClose={() => {
            // 「✕」= **真关闭**（只 ACK，不重试）。
            // 旧版这里 dispatch RETRY → 用户点「✕」以为关掉了，实际立刻又重下那个包。
            // ACK 后 e1.open=false → ErrorOverlay 回来（那里有重试/跳过/不重试返回）。
            rawE1({ type: "ACK" });
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
