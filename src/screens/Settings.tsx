/**
 * 设置页外壳：标题 + 返回 + 两个页签（通用 / 网络）。
 * 默认落在网络页——体检入口（E3）与「查看日志」都从这里进。
 */
import { useState } from "react";
import { useTranslation } from "react-i18next";
import SettingsGeneral from "./SettingsGeneral";
import SettingsNetwork from "./SettingsNetwork";

type Tab = "general" | "network";

export default function Settings({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const [tab, setTab] = useState<Tab>("network");

  const tabs: { id: Tab; labelKey: string }[] = [
    { id: "general", labelKey: "gen.title" },
    { id: "network", labelKey: "net.tab" },
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

      {tab === "general" ? <SettingsGeneral /> : <SettingsNetwork />}
    </div>
  );
}
