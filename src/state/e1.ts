/**
 * E1 内嵌诊断面板触发器：
 * EXECUTE 中**同一任务连续失败 2 次** → 面板滑出（不打断当前步骤上下文）。
 * 纯逻辑（可单测）：不同任务/成功会重置计数；触发后需 ack（关闭/重试）才可能再触发。
 */

export interface E1State {
  /** 当前计数任务 id */
  taskId: string | null;
  /** 该任务连续失败次数 */
  count: number;
  /** 面板是否打开 */
  open: boolean;
  /** 已触发待 ack（防止同一次失败反复弹） */
  armed: boolean;
}

export type E1Action =
  | { type: "FAIL"; taskId?: string } // 任务失败（ERROR 进入 EXECUTE 原态）
  | { type: "SUCCESS"; taskId?: string } // 任务完成 → 重置
  | { type: "ACK" } // 用户关闭/重试 → 收起面板
  | { type: "RESET" };

export const E1_THRESHOLD = 2;

export function createE1State(): E1State {
  return { taskId: null, count: 0, open: false, armed: false };
}

export function e1Reduce(s: E1State, a: E1Action): E1State {
  switch (a.type) {
    case "FAIL": {
      const tid = a.taskId ?? null;
      // 换了任务 → 重新计数；同任务连败累加
      const same = tid !== null && tid === s.taskId;
      const count = same ? s.count + 1 : 1;
      const shouldOpen = count >= E1_THRESHOLD && !s.armed;
      return {
        taskId: tid,
        count,
        open: shouldOpen ? true : s.open,
        armed: shouldOpen ? true : s.armed && same,
      };
    }
    case "SUCCESS":
      // 该任务成功 → 连败链断裂，全重置（换任务的场景也自然重置）
      if (a.taskId && s.taskId && a.taskId !== s.taskId) return s;
      return { ...createE1State() };
    case "ACK":
      return { ...s, open: false, armed: false, count: 0 };
    case "RESET":
      return createE1State();
    default:
      return s;
  }
}
