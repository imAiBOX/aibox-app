/**
 * S4 EXECUTE：当前任务大字 + 蓝色进度条 + 进度条上方通道徽章
 * + 已完成折叠列表（灰）+ 右下「查看日志」抽屉（等宽只读，log_line 环形缓冲 4KB）。
 * 失败态：红色横幅 + 人话原因 + 三按钮（重试/换源/导出诊断）由 ErrorOverlay 承载。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { currentTask, progress, type MachineState } from "../state";
import { formatBytes, formatEta, useDownloadProgress } from "../hooks/useDownloadProgress";
import { appLog, onEvent } from "../net/events";
import { logDrawer } from "../state/logDrawer";

const PHASE_KEYS: Record<string, string> = {
  download: "s4.phaseDownload",
  verify: "s4.phaseVerify",
  install: "s4.phaseInstall",
  probe: "s4.phaseProbe",
};

/** 下载线路代号 → 人话（Rust `ChannelType::as_str()`：DIRECT/MIRROR/PROXY/CUSTOM） */
const CHANNEL_KEYS: Record<string, string> = {
  DIRECT: "s4.channelDirect",
  MIRROR: "s4.channelMirror",
  PROXY: "s4.channelProxy",
  CUSTOM: "s4.channelCustom",
};

/** 阶段 → 一句话说明（这阶段到底在干什么、会不会动你的电脑） */
const PHASE_HINT_KEYS: Record<string, string> = {
  download: "s4.phaseHint.download",
  verify: "s4.phaseHint.verify",
  install: "s4.phaseHint.install",
  probe: "s4.phaseHint.probe",
};

/** 线路代号归一（Rust 大写；mock 可能小写） */
function channelKey(raw: string): string | null {
  const k = raw.trim().toUpperCase();
  return CHANNEL_KEYS[k] ?? null;
}

export default function S4Execute({ state }: { state: MachineState }) {
  const { t } = useTranslation();
  const [logsOpen, setLogsOpen] = useState(false);
  const [logLines, setLogLines] = useState<string[]>([]);
  const task = currentTask(state);
  const prog = progress(state);
  /** 已装好的 / 没装上的（跳过的）—— 旧版把两者混在「已完成」下面，
      ⊘ 没有图例，用户读成「完成了（打个叉？）」 */
  const doneItems = state.tasks.filter((tk) => tk.status === "done");
  const skippedItems = state.tasks.filter((tk) => tk.status === "skipped");
  const dl = useDownloadProgress();
  // 进度条数据源：download 阶段用事件（真分支 download_progress，jobId=任务 id），
  // 其余阶段/任务切换用状态机阶段进度（避免下载完成后 dl 残留 100%）
  const dlActive = !!task && task.phase === "download" && dl.pct > 0 && dl.jobId === task.id;
  /**
   * 总进度条**只用阶段步数**（prog.pct），**永不倒退**。
   *
   * 旧版：包内字节百分比（dl.pct）与全局步数百分比（prog.pct）塞进同一条条子 →
   * 某包下完瞬间是 100%，切到「校验中」又退回 5% → **进度条从满格倒回一小格**，
   * 用户以为「白下了 / 装坏了」。现在：条子只走全局步数，包内进度另起一行文字。
   */
  const barPct = prog.pct;
  /** 线路徽章：DIRECT/MIRROR/PROXY/CUSTOM → 人话（小白看到裸英文缩写只会懵） */
  const channelKeyRaw = dl.channel ? channelKey(dl.channel) : null;
  /** 包内进度（字节量 + 速度 + 剩余时间；只在下载中显示） */
  const packLine =
    dlActive && dl.total > 0
      ? `${formatBytes(dl.done)} / ${formatBytes(dl.total)}（${dl.pct}%）` +
        (dl.bps ? ` · ${formatBytes(dl.bps)}/s` : "") +
        (dl.etaSec !== null ? ` · ${t("s4.eta", { time: formatEta(dl.etaSec) ?? "" })}` : "")
      : null;

  // 日志抽屉：订阅 log_line → 环形缓冲（4KB 截断），只读展示
  useEffect(
    () =>
      onEvent((e) => {
        if (e.event === "log_line") setLogLines(appLog.lines());
      }),
    [],
  );

  // 外壳底栏的「查看日志」按钮 → 打开本页抽屉（跨组件开关）
  useEffect(() => logDrawer.subscribe(setLogsOpen), []);

  return (
    <div className="flex h-full flex-col gap-4">
      <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s4.title")}</h1>

      {task ? (
        <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-5">
          {/* 通道徽章（进度条上方，12px 等宽） */}
          <div className="mb-2 flex items-center gap-2">
            <span className="rounded-[3px] border border-aibox-line px-2 py-0.5 font-mono text-xs text-aibox-info">
              {channelKeyRaw ? t(channelKeyRaw) : t("s4.channelIdle")}
            </span>
            <span className="font-mono text-[11px] text-aibox-text2">
              {t(PHASE_KEYS[task.phase] ?? "s4.phaseDownload")}
            </span>
          </div>

          {/* 阶段人话说明（「校验中」「探测中」对小白是天书） */}
          <p className="mb-2 text-xs leading-relaxed text-aibox-text2">
            {t(PHASE_HINT_KEYS[task.phase] ?? "s4.phaseHint.download")}
          </p>

          {/* 当前任务大字 */}
          <div className="mb-3 flex items-baseline gap-3">
            <span className="font-display text-xl font-semibold text-aibox-text1">
              {task.name}
            </span>
            <span className="font-mono text-sm text-aibox-text2">{task.version}</span>
          </div>

          {/* 蓝色进度条 */}
          <div
            className="h-2 w-full overflow-hidden rounded-full bg-aibox-line"
            role="progressbar"
            aria-valuenow={prog.pct}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <div
              className="h-full rounded-full bg-aibox-info transition-[width] duration-200"
              style={{ width: `${barPct}%` }}
            />
          </div>
          {/* 包内进度（下载中）：已下载多少 / 一共多少 / 多快 / 还要多久 */}
          {packLine && (
            <p className="mt-1.5 font-mono text-[11px] text-aibox-text2">{packLine}</p>
          )}
          <div className="mt-1.5 flex justify-between font-mono text-[11px] text-aibox-text2">
            <span>
              {prog.done}/{prog.total} {t("s4.units")}
            </span>
            <span>{barPct}%</span>
          </div>
        </section>
      ) : (
        <p className="font-mono text-xs text-aibox-text2">{t("s4.allDone")}</p>
      )}

      {/* 已完成任务折叠列表（灰） */}
      {doneItems.length > 0 && (
        <details className="rounded-[6px] border border-aibox-line bg-aibox-raised px-4 py-2">
          <summary className="cursor-pointer py-1 font-mono text-xs text-aibox-text2">
            {t("s4.doneList")} ({doneItems.length})
          </summary>
          <ul className="flex flex-col gap-1 py-2">
            {doneItems.map((tk) => (
              <li
                key={tk.id}
                className="flex items-center justify-between font-mono text-xs text-aibox-text2 opacity-70"
              >
                <span>
                  √ {tk.name}
                </span>
                <span>{tk.version}</span>
              </li>
            ))}
          </ul>
        </details>
      )}

      {/* 没装上的（跳过）—— 单独一段并写明后果，不与「已完成」混在一起 */}
      {skippedItems.length > 0 && (
        <details className="rounded-[6px] border border-aibox-danger/50 bg-aibox-raised px-4 py-2">
          <summary className="cursor-pointer py-1 font-mono text-xs text-aibox-danger">
            {t("s4.skippedList")} ({skippedItems.length})
          </summary>
          <ul className="flex flex-col gap-1 py-2">
            {skippedItems.map((tk) => (
              <li
                key={tk.id}
                className="flex items-center justify-between font-mono text-xs text-aibox-text2"
              >
                <span>
                  ⊘ {tk.name}
                  <span className="ml-2 text-[10px] text-aibox-danger">{t("s7.skippedLabel")}</span>
                </span>
                <span>{tk.version}</span>
              </li>
            ))}
          </ul>
          <p className="pb-2 text-[11px] leading-relaxed text-aibox-text2">{t("s4.skippedHint")}</p>
        </details>
      )}

      {/* 右下「查看日志」抽屉（等宽只读） */}
      <div className="mt-auto flex justify-end">
        <button
          type="button"
          onClick={() => logDrawer.toggle()}
          className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          {t("s4.viewLog")}
        </button>
      </div>
      {logsOpen && (
        <div className="max-h-44 overflow-auto rounded-[6px] border border-aibox-line bg-aibox-bg p-3 font-mono text-[11px] leading-relaxed text-aibox-text2">
          {logLines.length === 0 ? (
            <p className="text-aibox-text2/60">{t("s4.logEmpty")}</p>
          ) : (
            logLines.map((line, i) => <p key={i}>{line}</p>)
          )}
          <p className="text-aibox-text2/60">{t("s4.logNote")}</p>
        </div>
      )}
    </div>
  );
}
