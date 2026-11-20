/**
 * 断点续装检测：启动读 persistence → 有未完成会话 → 弹「继续上次安装？」。
 * 换 SQLite 实现时只改注入的 persistence，不动本 hook。
 */
import { useEffect, useState } from "react";
import type { Event, MachineState, Persistence, SessionSnapshot } from "../state";

export interface SessionResumeState {
  /** 弹层是否显示 */
  asking: boolean;
  snapshot: SessionSnapshot | null;
  resume: () => void; // RESUME_SESSION
  dismiss: () => void; // DISMISS_RESUME
}

export function useSessionResume(
  persistence: Persistence,
  dispatch: (e: Event) => void,
  state: Pick<MachineState, "status" | "resume">,
  /**
   * 续装前的准备钩子（App 提供）：**必须重建 install plan**。
   *
   * 为什么必须：冷启动时 `state.plan` 是 null（plan 只由 PLAN_READY 产生），
   * 快照里只有 appId + doneTaskIds → 直接 RESUME_SESSION 会得到**空任务队列**
   * → 界面永远停在「安装中 / 全部任务完成」，用户无路可走（死胡同）。
   * 返回 false 表示准备失败（界面留在 IDLE，不进 EXECUTE）。
   */
  prepare?: (appId: string) => Promise<boolean>,
): SessionResumeState {
  const [snapshot, setSnapshot] = useState<SessionSnapshot | null>(null);

  useEffect(() => {
    let alive = true;
    void persistence.loadSession().then((snap) => {
      if (!alive || !snap || snap.doneTaskIds.length === 0) return;
      setSnapshot(snap);
      dispatch({ type: "DETECT_SESSION", snapshot: snap });
    });
    return () => {
      alive = false;
    };
    // 仅启动时检测一次
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return {
    asking: state.status === "IDLE" && state.resume.prompt && snapshot !== null,
    snapshot,
    resume: () => {
      const snap = snapshot;
      if (!snap) return;
      if (!prepare) {
        dispatch({ type: "RESUME_SESSION" });
        return;
      }
      void prepare(snap.appId).then((ok) => {
        if (ok) dispatch({ type: "RESUME_SESSION" });
      });
    },
    dismiss: () => dispatch({ type: "DISMISS_RESUME" }),
  };
}
