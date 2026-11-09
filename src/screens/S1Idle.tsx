/**
 * S1 IDLE 应用目录：卡片网格 auto-fill minmax(260px)，
 * 卡片 = 图标+名称+一句话+难度点+平台徽章+状态徽章；侧栏体检入口 E3。
 */
import { useTranslation } from "react-i18next";
import type { AppIndex } from "../net/client";
import type { ScanReport } from "../state";

function Difficulty({ n }: { n: number }) {
  const { t } = useTranslation();
  return (
    <span className="font-mono text-[11px] text-aibox-text2" title={t("s1.difficulty")}>
      {"●".repeat(n)}
      {"○".repeat(5 - n)}
    </span>
  );
}

function StatusBadge({ status, installed }: { status: string; installed?: string }) {
  const { t } = useTranslation();
  if (status === "installed") {
    return (
      <span className="rounded-[4px] border border-aibox-ok px-2 py-0.5 font-mono text-[10px] text-aibox-ok">
        {t("s1.statusInstalled")} {installed}
      </span>
    );
  }
  if (status === "checking") {
    return (
      <span className="rounded-[4px] border border-aibox-info px-2 py-0.5 font-mono text-[10px] text-aibox-info">
        {t("s1.statusChecking")}
      </span>
    );
  }
  return (
    <span className="rounded-[4px] border border-aibox-green px-2 py-0.5 font-mono text-[10px] text-aibox-green">
      {t("s1.statusInstallable")}
    </span>
  );
}

export default function S1Idle({
  index,
  scan,
  loading,
  selectedId,
  onSelect,
  onOpenDoctor,
}: {
  index: AppIndex | null;
  scan: ScanReport | null;
  loading: boolean;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onOpenDoctor: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div className="flex h-full gap-5">
      <div className="min-w-0 flex-1">
        <header className="mb-4 flex items-baseline justify-between">
          <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s1.title")}</h1>
          <span className="font-mono text-[11px] text-aibox-text2">{t("s1.subtitle")}</span>
        </header>

        {loading && <p className="font-mono text-xs text-aibox-text2">{t("s1.loading")}</p>}
        {!loading && (!index || index.apps.length === 0) && (
          <p className="font-mono text-xs text-aibox-text2">{t("s1.empty")}</p>
        )}

        <div className="grid gap-4 [grid-template-columns:repeat(auto-fill,minmax(260px,1fr))]">
          {index?.apps.map((a) => (
            <button
              key={a.id}
              type="button"
              onClick={() => onSelect(a.id)}
              className={[
                "flex flex-col gap-2 rounded-[6px] border bg-aibox-raised p-4 text-left transition-colors",
                selectedId === a.id
                  ? "border-aibox-green"
                  : "border-aibox-line hover:border-aibox-text2",
              ].join(" ")}
            >
              <div className="flex items-center justify-between">
                {/* 应用图标占位：像素方块母题 */}
                <span
                  aria-hidden
                  className="flex h-9 w-9 items-center justify-center rounded-[4px] bg-aibox-green font-display text-sm font-bold text-aibox-bg"
                >
                  {a.name.slice(0, 1)}
                </span>
                <StatusBadge status={a.status} installed={a.installedVersion} />
              </div>
              <span className="font-display text-base font-semibold text-aibox-text1">
                {a.name}
              </span>
              <span className="text-xs leading-relaxed text-aibox-text2">
                {t(a.summaryKey)}
              </span>
              <div className="mt-auto flex items-center justify-between gap-2 pt-1">
                <Difficulty n={a.difficulty} />
                <span className="flex gap-1">
                  {a.platforms.map((p) => (
                    <span
                      key={p}
                      className="rounded-[3px] border border-aibox-line px-1.5 py-0.5 font-mono text-[10px] text-aibox-text2"
                    >
                      {p === "win" ? "Windows" : p === "mac" ? "macOS" : p}
                    </span>
                  ))}
                </span>
              </div>
            </button>
          ))}
        </div>
      </div>

      {/* 侧栏体检入口 E3 */}
      <aside className="hidden w-52 shrink-0 flex-col gap-3 border-l border-aibox-line pl-4 lg:flex">
        <h2 className="font-display text-sm font-semibold text-aibox-cream">{t("s1.doctorTitle")}</h2>
        <p className="text-xs leading-relaxed text-aibox-text2">{t("s1.doctorDesc")}</p>
        {scan && (
          <div className="flex flex-col gap-1.5 font-mono text-[11px]">
            {scan.items.slice(0, 5).map((it) => (
              <span key={it.id} className="flex items-center justify-between">
                <span className="text-aibox-text2">{t(it.labelKey)}</span>
                <span className={it.ok ? "text-aibox-ok" : "text-aibox-danger"}>
                  {it.ok ? "√" : "!"} {it.value ?? ""}
                </span>
              </span>
            ))}
          </div>
        )}
        <button
          type="button"
          onClick={onOpenDoctor}
          className="mt-auto rounded-[4px] border border-aibox-line px-3 py-2 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green"
        >
          {t("s1.doctorOpen")}
        </button>
      </aside>
    </div>
  );
}
