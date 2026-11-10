/**
 * S4 EXECUTE：当前任务大字 + 蓝色进度条 + 进度条上方通道徽章
 * + 已完成折叠列表（灰）+ 右下「查看日志」抽屉（等宽只读，log_line 环形缓冲 4KB）。
 * 失败态：红色横幅 + 人话原因 + 三按钮（重试/换源/导出诊断）由 ErrorOverlay 承载。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { completedTasks, currentTask, progress, type MachineState } from "../state";
import { useDownloadProgress } from "../hooks/useDownloadProgress";
import { appLog, onEvent } from "../net/events";

const PHASE_KEYS: Record<string, string> = {
  download: "s4.phaseDownload",
  verify: "s4.phaseVerify",
  install: "s4.phaseInstall",
  probe: "s4.phaseProbe",
};

export default function S4Execute({ state }: { state: MachineState }) {
  const { t } = useTranslation();
  const [logsOpen, setLogsOpen] = useState(false);
  const [logLines, setLogLines] = useState<string[]>([]);
  const task = currentTask(state);
  const prog = progress(state);
  const done = completedTasks(state);
  const dl = useDownloadProgress();
  // 进度条数据源：download 阶段用事件（真分支 download_progress，jobId=任务 id），
  // 其余阶段/任务切换用状态机阶段进度（避免下载完成后 dl 残留 100%）
  const dlActive = !!task && task.phase === "download" && dl.pct > 0 && dl.jobId === task.id;
  const barPct = dlActive ? dl.pct : prog.pct;

  // 日志抽屉：订阅 log_line → 环形缓冲（4KB 截断），只读展示
  useEffect(
    () =>
      onEvent((e) => {
        if (e.event === "log_line") setLogLines(appLog.lines());
      }),
    [],
  );

  return (
    <div className="flex h-full flex-col gap-4">
      <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s4.title")}</h1>

      {task ? (
        <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-5">
          {/* 通道徽章（进度条上方，12px 等宽） */}
          <div className="mb-2 flex items-center gap-2">
            <span className="rounded-[3px] border border-aibox-line px-2 py-0.5 font-mono text-xs text-aibox-info">
              {dlActive && dl.channel ? dl.channel : t("s4.channelIdle")}
            </span>
            <span className="font-mono text-[11px] text-aibox-text2">
              {t(PHASE_KEYS[task.phase] ?? "s4.phaseDownload")}
            </span>
          </div>

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
      {done.length > 0 && (
        <details className="rounded-[6px] border border-aibox-line bg-aibox-raised px-4 py-2">
          <summary className="cursor-pointer py-1 font-mono text-xs text-aibox-text2">
            {t("s4.doneList")} ({done.length})
          </summary>
          <ul className="flex flex-col gap-1 py-2">
            {done.map((tk) => (
              <li
                key={tk.id}
                className="flex items-center justify-between font-mono text-xs text-aibox-text2 opacity-70"
              >
                <span>
                  {tk.status === "skipped" ? "⊘" : "√"} {tk.name}
                </span>
                <span>{tk.version}</span>
              </li>
            ))}
          </ul>
        </details>
      )}

      {/* 右下「查看日志」抽屉（等宽只读） */}
      <div className="mt-auto flex justify-end">
        <button
          type="button"
          onClick={() => setLogsOpen((v) => !v)}
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
