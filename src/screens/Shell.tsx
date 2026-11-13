/**
 * 全局布局骨架：左侧 64px 导航（六态圆点+当前态）+ 主区 + 底部 48px 操作条。
 * 主按钮右对齐、危险操作左对齐。
 */
import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { NAV_STEPS, navIndex, type Status } from "../state";
import { isMacPlatform } from "../platform";

export interface BarAction {
  labelKey: string;
  onClick: () => void;
  disabled?: boolean;
  /** 危险操作 → 左对齐 + danger 色 */
  danger?: boolean;
  primary?: boolean;
}

const NAV_KEYS: Record<string, string> = {
  IDLE: "nav.idle",
  SCAN: "nav.scan",
  PLAN: "nav.plan",
  EXECUTE: "nav.execute",
  CONFIG: "nav.config",
  VERIFY: "nav.verify",
};

export function LeftNav({
  status,
  errorFrom,
  onOpenSettings,
  onOpenSmartScreen,
  onOpenManage,
}: {
  status: Status;
  errorFrom?: Status;
  onOpenSettings: () => void;
  onOpenSmartScreen: () => void;
  onOpenManage: () => void;
}) {
  const { t } = useTranslation();
  const active = navIndex(status, errorFrom);
  return (
    <nav
      className="flex w-16 shrink-0 flex-col items-center gap-4 border-r border-aibox-line bg-aibox-raised py-4"
      aria-label="steps"
    >
      <span className="font-display text-[10px] font-bold tracking-widest text-aibox-green">
        AI
      </span>
      <ol className="flex flex-1 flex-col items-center gap-5 pt-2">
        {NAV_STEPS.map((s, i) => {
          const isActive = i === active;
          const isDone = i < active;
          return (
            <li key={s} className="flex flex-col items-center gap-1" title={t(NAV_KEYS[s])}>
              <span
                className={[
                  "h-3 w-3 rounded-full border",
                  isActive
                    ? "border-aibox-green bg-aibox-green"
                    : isDone
                      ? "border-aibox-ok bg-aibox-ok"
                      : "border-aibox-line bg-transparent",
                ].join(" ")}
                aria-current={isActive ? "step" : undefined}
              />
              <span
                className={[
                  "font-mono text-[9px]",
                  isActive ? "text-aibox-green" : "text-aibox-text2",
                ].join(" ")}
              >
                {i + 1}
              </span>
            </li>
          );
        })}
      </ol>
      <div className="flex flex-col items-center gap-3">
        <button
          type="button"
          onClick={onOpenManage}
          title={t("nav.manage")}
          className="flex h-8 w-8 items-center justify-center rounded-[4px] border border-aibox-line font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          ⊞
        </button>
        <button
          type="button"
          onClick={onOpenSettings}
          title={t("nav.settings")}
          className="flex h-8 w-8 items-center justify-center rounded-[4px] border border-aibox-line font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          ⚙
        </button>
        <button
          type="button"
          onClick={onOpenSmartScreen}
          title={t(isMacPlatform() ? "nav.gatekeeper" : "nav.smartscreen")}
          className="flex h-8 w-8 items-center justify-center rounded-[4px] border border-aibox-line font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          ?
        </button>
      </div>
    </nav>
  );
}

export function BottomBar({ actions }: { actions: BarAction[] }) {
  const { t } = useTranslation();
  const danger = actions.filter((a) => a.danger);
  const normal = actions.filter((a) => !a.danger);
  const render = (a: BarAction) => (
    <button
      key={a.labelKey + (a.danger ? "-d" : "-n")}
      type="button"
      onClick={a.onClick}
      disabled={a.disabled}
      className={[
        "rounded-[4px] px-5 py-2 font-display text-sm font-semibold transition-colors disabled:cursor-not-allowed disabled:opacity-40",
        a.primary
          ? "bg-aibox-green text-aibox-bg hover:bg-aibox-green-d"
          : a.danger
            ? "border border-aibox-danger text-aibox-danger hover:bg-aibox-danger hover:text-aibox-bg"
            : "border border-aibox-line text-aibox-text1 hover:border-aibox-green hover:text-aibox-green",
      ].join(" ")}
    >
      {t(a.labelKey)}
    </button>
  );
  return (
    <footer className="flex h-12 shrink-0 items-center justify-between border-t border-aibox-line bg-aibox-raised px-4">
      <div className="flex gap-2">{danger.map(render)}</div>
      <div className="flex gap-2">{normal.map(render)}</div>
    </footer>
  );
}

export function AppShell({
  status,
  errorFrom,
  bar,
  onOpenSettings,
  onOpenSmartScreen,
  onOpenManage,
  banner,
  children,
}: {
  status: Status;
  errorFrom?: Status;
  bar: BarAction[];
  onOpenSettings: () => void;
  onOpenSmartScreen: () => void;
  onOpenManage: () => void;
  banner?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="flex h-screen w-screen overflow-hidden bg-aibox-bg text-aibox-text1">
      <LeftNav
        status={status}
        errorFrom={errorFrom}
        onOpenSettings={onOpenSettings}
        onOpenSmartScreen={onOpenSmartScreen}
        onOpenManage={onOpenManage}
      />
      <div className="flex min-w-0 flex-1 flex-col">
        {banner}
        <main className="min-h-0 flex-1 overflow-y-auto px-6 py-5">{children}</main>
        <BottomBar actions={bar} />
      </div>
    </div>
  );
}
