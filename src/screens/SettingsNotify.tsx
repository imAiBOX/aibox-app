/**
 * 设置-通知页：用户对「提醒什么」的最终决定权。
 *
 * **为什么单独一页**：AiBOX 常驻后，提示能力是「有用」与「烦人」之间的平衡。
 * 全部塞进「通用」页会被忽略 → 用户要么全关（价值归零）、要么全开（被通知轰炸）。
 * 单独一页 + 每项一句人话说明 + 明确默认值，用户才能做出自己的选择。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { DEFAULT_NOTIFY_PREFS, type NotifyPrefs } from "../state/notifyPrefs";
import { notificationAllowed } from "../lib/notify";

interface Props {
  prefs: NotifyPrefs;
  onChange: (p: NotifyPrefs) => void;
  /** 托盘在哪（人话说明；按平台自适应） */
  trayHint: string;
}

/** 小时下拉（0-23） */
const HOURS = Array.from({ length: 24 }, (_, i) => i);

function Row({
  title,
  desc,
  checked,
  onToggle,
}: {
  title: string;
  desc: string;
  checked: boolean;
  onToggle: (v: boolean) => void;
}) {
  return (
    <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
      <div className="flex items-center justify-between gap-3">
        <h3 className="font-display text-sm font-semibold text-aibox-cream">{title}</h3>
        <button
          type="button"
          role="switch"
          aria-checked={checked}
          aria-label={title}
          onClick={() => onToggle(!checked)}
          className={[
            "h-6 w-11 shrink-0 rounded-full border transition-colors",
            checked
              ? "border-aibox-green bg-aibox-green/30"
              : "border-aibox-line bg-aibox-bg",
          ].join(" ")}
        >
          <span
            className={[
              "block h-4 w-4 rounded-full transition-transform",
              checked ? "translate-x-6 bg-aibox-green" : "translate-x-1 bg-aibox-text2",
            ].join(" ")}
          />
        </button>
      </div>
      <p className="mt-2 text-xs leading-relaxed text-aibox-text2">{desc}</p>
    </section>
  );
}

export default function SettingsNotify({ prefs, onChange, trayHint }: Props) {
  const { t } = useTranslation();
  const [p, setP] = useState<NotifyPrefs>(prefs);
  useEffect(() => setP(prefs), [prefs]);
  const set =
    <K extends keyof NotifyPrefs>(k: K) =>
    (v: NotifyPrefs[K]) => {
      const next = { ...p, [k]: v };
      setP(next);
      onChange(next);
    };
  // 系统通知授权状态（用户可能一开始拒绝、后来才允许 → 页面要能看出��）
  const [allowed, setAllowed] = useState<boolean | null>(null);
  useEffect(() => {
    void notificationAllowed().then(setAllowed);
  }, []);
  return (
    <div className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto pr-1">
      {allowed === false && (
        <p className="rounded-[4px] border border-aibox-danger/60 bg-aibox-danger/10 px-3 py-2 text-xs leading-relaxed text-aibox-text2">
          {t("notify.permissionDenied")}
        </p>
      )}
      <p className="text-xs leading-relaxed text-aibox-text2">{t("notify.intro", { where: trayHint })}</p>
      <Row
        title={t("notify.coreUpdate")}
        desc={t("notify.coreUpdateDesc")}
        checked={p.coreUpdate}
        onToggle={set("coreUpdate")}
      />
      <Row
        title={t("notify.appUpdate")}
        desc={t("notify.appUpdateDesc")}
        checked={p.appUpdate}
        onToggle={set("appUpdate")}
      />
      <Row
        title={t("notify.newApps")}
        desc={t("notify.newAppsDesc")}
        checked={p.newApps}
        onToggle={set("newApps")}
      />
      {/* 免打扰时段：夜里不弹，醒来再提醒（不标记已通知 → 补发） */}
      <section className="rounded-[6px] border border-aibox-line bg-aibox-raised p-4">
        <div className="flex items-center justify-between">
          <h3 className="font-display text-sm font-semibold text-aibox-cream">
            {t("notify.quiet")}
          </h3>
          <button
            type="button"
            role="switch"
            aria-checked={p.quietEnabled}
            aria-label={t("notify.quiet")}
            onClick={() => set("quietEnabled")(!p.quietEnabled)}
            className={[
              "h-6 w-11 shrink-0 rounded-full border transition-colors",
              p.quietEnabled
                ? "border-aibox-green bg-aibox-green/30"
                : "border-aibox-line bg-aibox-bg",
            ].join(" ")}
          >
            <span
              className={[
                "block h-4 w-4 rounded-full transition-transform",
                p.quietEnabled ? "translate-x-6 bg-aibox-green" : "translate-x-1 bg-aibox-text2",
              ].join(" ")}
            />
          </button>
        </div>
        <p className="mt-2 text-xs leading-relaxed text-aibox-text2">{t("notify.quietDesc")}</p>
        {p.quietEnabled && (
          <div className="mt-3 flex flex-wrap items-center gap-2">
            <label className="flex items-center gap-2 text-xs text-aibox-text2">
              {t("notify.quietFrom")}
              <select
                value={p.quietStart}
                onChange={(e) => set("quietStart")(Number(e.target.value))}
                className="rounded-[4px] border border-aibox-line bg-aibox-bg px-2 py-1 font-mono text-xs text-aibox-text1"
              >
                {HOURS.map((h) => (
                  <option key={h} value={h}>
                    {String(h).padStart(2, "0")}:00
                  </option>
                ))}
              </select>
            </label>
            <label className="flex items-center gap-2 text-xs text-aibox-text2">
              {t("notify.quietTo")}
              <select
                value={p.quietEnd}
                onChange={(e) => set("quietEnd")(Number(e.target.value))}
                className="rounded-[4px] border border-aibox-line bg-aibox-bg px-2 py-1 font-mono text-xs text-aibox-text1"
              >
                {HOURS.map((h) => (
                  <option key={h} value={h}>
                    {String(h).padStart(2, "0")}:00
                  </option>
                ))}
              </select>
            </label>
          </div>
        )}
      </section>

      <Row
        title={t("notify.startMinimized")}
        desc={t("notify.startMinimizedDesc", { where: trayHint })}
        checked={p.startMinimized}
        onToggle={set("startMinimized")}
      />
      <Row
        title={t("notify.minimizeAfterInstall")}
        desc={t("notify.minimizeAfterInstallDesc", { where: trayHint })}
        checked={p.minimizeAfterInstall}
        onToggle={set("minimizeAfterInstall")}
      />
      <button
        type="button"
        onClick={() => {
          const d = { ...DEFAULT_NOTIFY_PREFS };
          setP(d);
          onChange(d);
        }}
        className="self-start rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
      >
        {t("notify.reset")}
      </button>
    </div>
  );
}
