/**
 * 日志抽屉开关（全局单例）。
 *
 * **为什么需要它**：底栏「查看日志」按钮在 App.tsx（外壳层），
 * 而真正的日志抽屉在 S4Execute.tsx（页面层）内部。
 * 旧版按钮直接 `setOverlay("settings")` —— 打开的是设置页，里面根本没有日志，
 * 用户在最需要自救的时候点进一个空页面。
 *
 * 这里用一个跨组件的开关把两者接起来：外壳说「打开日志」，页面自己开抽屉。
 */

export interface LogDrawerController {
  /** 当前是否打开 */
  isOpen(): boolean;
  open(): void;
  close(): void;
  toggle(): void;
  /** 订阅开关变化；返回退订 */
  subscribe(fn: (open: boolean) => void): () => void;
}

export function createLogDrawerController(): LogDrawerController {
  let open = false;
  const subs = new Set<(v: boolean) => void>();
  const notify = () => {
    for (const fn of [...subs]) fn(open);
  };
  return {
    isOpen: () => open,
    open() {
      if (open) return;
      open = true;
      notify();
    },
    close() {
      if (!open) return;
      open = false;
      notify();
    },
    toggle() {
      open = !open;
      notify();
    },
    subscribe(fn) {
      subs.add(fn);
      fn(open);
      return () => subs.delete(fn);
    },
  };
}

/** 全局单例（App 触发打开，S4Execute 订阅渲染） */
export const logDrawer = createLogDrawerController();
