/**
 * 断点续装持久化接口。
 * 接口先定义；真实现（tauri-plugin-sql SQLite）后续接入，此处提供：
 *  - memoryPersistence：单测 / mock 模式用
 *  - localStoragePersistence：纯 web 开发兜底（非 Tauri 环境）
 */
import type { SessionSnapshot } from "./types";

export interface Persistence {
  loadSession(): Promise<SessionSnapshot | null>;
  saveSnapshot(snap: SessionSnapshot): Promise<void>;
  clearSession(): Promise<void>;
}

/** 内存实现：单测与 VITE_AIBOX_MOCK=1 用 */
export function memoryPersistence(
  initial: SessionSnapshot | null = null,
): Persistence & { readonly store: { snap: SessionSnapshot | null } } {
  const store = { snap: initial };
  return {
    store,
    async loadSession() {
      return store.snap;
    },
    async saveSnapshot(snap) {
      store.snap = snap;
    },
    async clearSession() {
      store.snap = null;
    },
  };
}

const LS_KEY = "aibox.session";

/** localStorage 实现：无 Tauri/SQLite 时的降级（web dev / CI） */
export const localStoragePersistence: Persistence = {
  async loadSession() {
    try {
      const raw = localStorage.getItem(LS_KEY);
      return raw ? (JSON.parse(raw) as SessionSnapshot) : null;
    } catch {
      return null;
    }
  },
  async saveSnapshot(snap) {
    try {
      localStorage.setItem(LS_KEY, JSON.stringify(snap));
    } catch {
      /* 存储不可用：静默降级（不阻塞安装流程） */
    }
  },
  async clearSession() {
    try {
      localStorage.removeItem(LS_KEY);
    } catch {
      /* ignore */
    }
  },
};

/** 从快照推导恢复用的已完成任务 id 列表 */
export function doneIdsOf(snap: SessionSnapshot | null): string[] {
  return snap ? snap.doneTaskIds : [];
}
