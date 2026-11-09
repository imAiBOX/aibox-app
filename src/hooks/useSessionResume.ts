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
    resume: () => dispatch({ type: "RESUME_SESSION" }),
    dismiss: () => dispatch({ type: "DISMISS_RESUME" }),
  };
}
