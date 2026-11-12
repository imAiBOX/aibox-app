/**
 * S1 IDLE 应用目录：卡片网格 auto-fill minmax(260px)，
 * 卡片 = 图标+名称+一句话+难度点+平台徽章+状态徽章（+可升级徽章）；侧栏体检入口 E3。
 * 顶部：已装应用更新聚合条（可展开跳卡片）+ 目录新增提示（可关）。
 */
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { AppIndex, AppIndexEntry, AppUpdateRow } from "../net/client";
import {
  ALL_CATEGORIES,
  deriveCategories,
  filterApps,
  sidebarScanRows,
  type ScanReport,
} from "../state";

function Difficulty({ n }: { n: number }) {
  const { t } = useTranslation();
  const dots = Math.max(0, Math.min(5, Math.round(n)));
  return (
    <span className="font-mono text-[11px] text-aibox-text2" title={t("s1.difficulty")}>
      {"●".repeat(dots)}
      {"○".repeat(5 - dots)}
    </span>
  );
}

/** 徽章派生（registry 真数据 → 三态）：已装 > stable 可安装 > 版本核对中 */
export function appBadge(a: AppIndexEntry): "installed" | "installable" | "checking" {
  if (a.installedVersion) return "installed";
  return a.status === "stable" ? "installable" : "checking";
}

/** 一句话简介：tagline 按当前语言取，回落 en / 空 */
function taglineOf(a: AppIndexEntry, lang: string): string {
  const tl = a.tagline;
  if (!tl) return "";
  return tl[lang] ?? tl[lang.split("-")[0] ?? ""] ?? tl.en ?? Object.values(tl)[0] ?? "";
}

function StatusBadge({ entry, upgradeTo }: { entry: AppIndexEntry; upgradeTo?: string }) {
  const { t } = useTranslation();
  const kind = appBadge(entry);
  return (
    <span className="flex flex-wrap items-center justify-end gap-1.5">
      {kind === "installed" && (
        <span className="rounded-[4px] border border-aibox-ok px-2 py-0.5 font-mono text-[10px] text-aibox-ok">
          {t("s1.statusInstalled")} {entry.installedVersion}
        </span>
      )}
      {kind === "checking" && (
        <span className="rounded-[4px] border border-aibox-info px-2 py-0.5 font-mono text-[10px] text-aibox-info">
          {t("s1.statusChecking")}
        </span>
      )}
      {kind === "installable" && (
        <span className="rounded-[4px] border border-aibox-green px-2 py-0.5 font-mono text-[10px] text-aibox-green">
          {t("s1.statusInstallable")}
        </span>
      )}
      {/* 可升级徽章（info 色，区别于「已装」绿徽章） */}
      {upgradeTo && (
        <span className="rounded-[4px] border border-aibox-info bg-aibox-info/10 px-2 py-0.5 font-mono text-[10px] text-aibox-info">
          {t("s1.upgradeTo", { version: upgradeTo })}
        </span>
      )}
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
  appUpdates,
  newApps,
  onDismissNewApps,
}: {
  index: AppIndex | null;
  scan: ScanReport | null;
  loading: boolean;
  selectedId: string | null;
  onSelect: (id: string) => void;
  onOpenDoctor: () => void;
  /** check_app_updates 结果（有 hasUpdate 的行 → 徽章/聚合条） */
  appUpdates: AppUpdateRow[];
  /** 目录新增的 app id（首启为空；可关） */
  newApps: string[];
  onDismissNewApps: () => void;
}) {
  const { t, i18n } = useTranslation();
  const lang = i18n.resolvedLanguage ?? "en";
  const [updatesOpen, setUpdatesOpen] = useState(false);
  /** 分类 chips（默认 All）+ 搜索框；过滤经 175ms 防抖后与分类组合生效 */
  const [category, setCategory] = useState(ALL_CATEGORIES);
  const [query, setQuery] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");

  useEffect(() => {
    const id = window.setTimeout(() => setDebouncedQuery(query), 175);
    return () => window.clearTimeout(id);
  }, [query]);

  const allApps = index?.apps ?? [];
  /** 先分类后搜索的组合过滤（纯函数，见 state/appFilter） */
  const visible = filterApps(allApps, debouncedQuery, category);
  /** chips 数据源从 index 推导（不硬编码分类枚举）；计数随搜索收窄 */
  const searchHit = filterApps(allApps, debouncedQuery, ALL_CATEGORIES);
  const chips = [
    { id: ALL_CATEGORIES, count: searchHit.length },
    ...deriveCategories(searchHit),
  ];
  /** 一键清空：搜索 + 分类同时复位 */
  const clearFilters = () => {
    setQuery("");
    setDebouncedQuery("");
    setCategory(ALL_CATEGORIES);
  };

  const nameOf = (id: string): string => index?.apps.find((a) => a.id === id)?.name ?? id;
  const updatable = appUpdates.filter((r) => r.hasUpdate);

  /** 展开列表点项 → 跳到对应卡片（选中 + 滚动居中） */
  const jumpTo = (id: string) => {
    onSelect(id);
    window.requestAnimationFrame(() =>
      document.getElementById(`app-card-${id}`)?.scrollIntoView({ behavior: "smooth", block: "center" }),
    );
  };

  return (
    <div className="flex h-full gap-5">
      <div className="min-w-0 flex-1">
        <header className="mb-4 flex items-baseline justify-between">
          <h1 className="font-display text-xl font-bold text-aibox-cream">{t("s1.title")}</h1>
          <span className="font-mono text-[11px] text-aibox-text2">{t("s1.subtitle")}</span>
        </header>

        {/* 已装应用更新聚合条（黄条；点击展开列表 → 跳卡片；本批只提醒+入口） */}
        {updatable.length > 0 && (
          <div className="mb-3 rounded-[6px] border border-[#f5c518] bg-[#f5c518]/15 px-4 py-2 text-sm text-aibox-cream">
            <button
              type="button"
              className="flex w-full items-center justify-between gap-3 text-left"
              onClick={() => setUpdatesOpen((o) => !o)}
              aria-expanded={updatesOpen}
            >
              <span>{t("s1.updatesBar", { count: updatable.length })}</span>
              <span className="font-mono text-[11px] text-aibox-text2">
                {updatesOpen ? "▲" : "▼"}
              </span>
            </button>
            {updatesOpen && (
              <ul className="mt-2 flex flex-col gap-1.5">
                {updatable.map((r) => (
                  <li key={r.appId}>
                    <button
                      type="button"
                      className="flex w-full items-center justify-between gap-3 rounded-[4px] border border-aibox-line bg-aibox-bg px-3 py-1.5 text-left text-xs hover:border-aibox-green"
                      onClick={() => jumpTo(r.appId)}
                    >
                      <span className="text-aibox-text1">{nameOf(r.appId)}</span>
                      <span className="font-mono text-[10px] text-aibox-green">
                        {t("s1.upgradeTo", { version: r.latest ?? "?" })}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}

        {/* 目录新增提示（人话，可关） */}
        {newApps.length > 0 && (
          <div className="mb-3 flex items-center justify-between gap-3 rounded-[6px] border border-aibox-info/60 bg-aibox-info/10 px-4 py-2 text-xs text-aibox-info">
            <span>
              {t("s1.newApps", {
                names: newApps.map(nameOf).join(lang.startsWith("zh") ? "、" : ", "),
              })}
            </span>
            <button
              type="button"
              onClick={onDismissNewApps}
              title={t("s1.dismiss")}
              aria-label={t("s1.dismiss")}
              className="shrink-0 rounded-[3px] px-1.5 font-mono text-sm text-aibox-info hover:text-aibox-cream"
            >
              ×
            </button>
          </div>
        )}

        {loading && <p className="font-mono text-xs text-aibox-text2">{t("s1.loading")}</p>}
        {!loading && (!index || index.apps.length === 0) && (
          <p className="font-mono text-xs text-aibox-text2">{t("s1.empty")}</p>
        )}

        {/* 搜索 + 分类 chips（黑底绿 accent、圆角≤8、等宽 placeholder；组合过滤） */}
        {!loading && index && index.apps.length > 0 && (
          <div className="mb-4 flex flex-col gap-2.5">
            <div className="relative w-full sm:max-w-xs">
              <input
                type="text"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder={t("s1.searchPlaceholder")}
                aria-label={t("s1.searchPlaceholder")}
                className="w-full rounded-[4px] border border-aibox-line bg-aibox-bg py-2 pl-3 pr-8 font-mono text-sm text-aibox-text1 outline-none placeholder:text-aibox-text2 focus:border-aibox-green"
              />
              {query && (
                <button
                  type="button"
                  onClick={() => setQuery("")}
                  title={t("s1.searchClear")}
                  aria-label={t("s1.searchClear")}
                  className="absolute right-2 top-1/2 -translate-y-1/2 rounded-[3px] px-1 font-mono text-sm text-aibox-text2 hover:text-aibox-green"
                >
                  ×
                </button>
              )}
            </div>
            <div className="flex flex-wrap items-center gap-1.5">
              {chips.map((c) => {
                const active = category === c.id;
                const label =
                  c.id === ALL_CATEGORIES ? t("s1.all") : t(`app.category.${c.id}`, { defaultValue: c.id });
                return (
                  <button
                    key={c.id}
                    type="button"
                    onClick={() => setCategory(c.id)}
                    aria-pressed={active}
                    className={[
                      "rounded-[4px] border px-2.5 py-1 text-xs transition-colors",
                      active
                        ? "border-aibox-green bg-aibox-green/10 text-aibox-green"
                        : "border-aibox-line text-aibox-text2 hover:border-aibox-text2 hover:text-aibox-text1",
                    ].join(" ")}
                  >
                    {label}
                    <span className="ml-1.5 font-mono text-[10px] opacity-70">{c.count}</span>
                  </button>
                );
              })}
            </div>
          </div>
        )}

        {/* 空态人话：有过滤条件但无命中 → 一键清空 */}
        {!loading && index && index.apps.length > 0 && visible.length === 0 && (
          <div className="rounded-[6px] border border-aibox-line bg-aibox-raised px-4 py-6 text-center">
            <p className="text-sm text-aibox-text2">{t("s1.noMatches")}</p>
            <button
              type="button"
              onClick={clearFilters}
              className="mt-3 rounded-[4px] border border-aibox-green px-3 py-1.5 font-mono text-xs text-aibox-green hover:bg-aibox-green/10"
            >
              {t("s1.searchClear")}
            </button>
          </div>
        )}

        <div className="grid gap-4 [grid-template-columns:repeat(auto-fill,minmax(260px,1fr))]">
          {visible.map((a) => (
            <button
              key={a.id}
              id={`app-card-${a.id}`}
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
                <StatusBadge entry={a} upgradeTo={appUpdates.find((r) => r.hasUpdate && r.appId === a.id)?.latest} />
              </div>
              <span className="font-display text-base font-semibold text-aibox-text1">
                {a.name}
              </span>
              <span className="text-xs leading-relaxed text-aibox-text2">
                {taglineOf(a, lang)}
              </span>
              <div className="mt-auto flex items-center justify-between gap-2 pt-1">
                <Difficulty n={a.difficulty ?? 3} />
                <span className="flex gap-1">
                  {a.platforms.map((p) => (
                    <span
                      key={p}
                      className="rounded-[3px] border border-aibox-line px-1.5 py-0.5 font-mono text-[10px] text-aibox-text2"
                    >
                      {p === "windows" ? "Windows" : p === "macos" ? "macOS" : p}
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
            {sidebarScanRows(scan).map((it) => (
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
