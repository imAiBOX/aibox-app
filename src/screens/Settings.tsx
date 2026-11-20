/**
 * 设置页外壳：标题 + 返回 + 两个页签（通用 / 网络）。
 * 默认落在网络页——体检入口（E3）与「查看日志」都从这里进。
 */
import { useState } from "react";
import { useTranslation } from "react-i18next";
import SettingsGeneral from "./SettingsGeneral";
import SettingsNetwork from "./SettingsNetwork";
import SettingsNotify from "./SettingsNotify";
import { DEFAULT_NOTIFY_PREFS, type NotifyPrefs } from "../state/notifyPrefs";

type Tab = "general" | "network" | "notify";

export default function Settings({
  onClose,
  autoRunDoctor,
  notifyPrefs,
  onNotifyPrefsChange,
  trayHint,
}: {
  onClose: () => void;
  /** 打开即自动跑一次网络体检（首屏侧栏「打开体检」用：承诺「看看你的电脑」就要真看） */
  autoRunDoctor?: boolean;
  /** 通知偏好（用户说了算） */
  notifyPrefs?: NotifyPrefs;
  onNotifyPrefsChange?: (p: NotifyPrefs) => void;
  /** 「AiBOX 在哪」的人话说明（按平台自适应） */
  trayHint?: string;
}) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("network");

  const tabs: { id: Tab; labelKey: string }[] = [
    { id: "general", labelKey: "gen.title" },
    { id: "network", labelKey: "net.tab" },
    { id: "notify", labelKey: "notify.tab" },
  ];

  return (
    <div className="mx-auto flex h-full max-w-xl flex-col gap-4">
      <header className="flex items-center justify-between">
        <h1 className="font-display text-xl font-bold text-aibox-cream">{t("set.title")}</h1>
        <button
          type="button"
          onClick={onClose}
          className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          {t("net.back")}
        </button>
      </header>

      <div className="flex gap-2">
        {tabs.map((x) => (
          <button
            key={x.id}
            type="button"
            onClick={() => setTab(x.id)}
            className={[
              "rounded-[4px] border px-3 py-1.5 font-mono text-xs",
              tab === x.id
                ? "border-aibox-green text-aibox-green"
                : "border-aibox-line text-aibox-text2",
            ].join(" ")}
          >
            {t(x.labelKey)}
          </button>
        ))}
      </div>

      {tab === "general" ? (
        <SettingsGeneral />
      ) : tab === "notify" ? (
        <SettingsNotify
          prefs={notifyPrefs ?? DEFAULT_NOTIFY_PREFS}
          onChange={onNotifyPrefsChange ?? (() => undefined)}
          trayHint={trayHint ?? ""}
        />
      ) : (
        <SettingsNetwork autoRun={autoRunDoctor} />
      )}
    </div>
  );
}
