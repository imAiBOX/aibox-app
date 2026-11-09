/**
 * 诊断导出控件（ErrorOverlay 与 SettingsNetwork 共用）：
 * 「导出诊断」→ diag_export（zip 落桌面，Rust 已脱敏）→ 展示 zip 路径
 * +「一键新建 GitHub Issue」（预填人话摘要，前端再过一遍脱敏）。
 * 导出中 loading；失败人话。不自动 dispatch 状态机事件（EXPORT_DIAG 由调用方决定）。
 */
import { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../net/client";
import { buildIssueUrl } from "../net/issue";

export interface DiagExportProps {
  /** 当前语言（Issue body 模板用） */
  lang?: string;
  /** OS 描述（Issue 预填；缺省从 navigator 粗略取） */
  os?: string;
  /** 错误码（ErrorOverlay 传入；设置页可空） */
  errorCode?: string;
  /** 人话现象（已由调用方 i18n 好） */
  symptom?: string;
  /** 导出成功回调（可选；ErrorOverlay 用它 dispatch EXPORT_DIAG） */
  onExported?: (zipPath: string) => void;
  /** 紧凑模式（ErrorOverlay 内嵌用） */
  compact?: boolean;
}

function basename(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}

export default function DiagExport({
  lang,
  os,
  errorCode,
  symptom,
  onExported,
  compact,
}: DiagExportProps) {
  const { t, i18n } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [zipPath, setZipPath] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const effectiveLang = lang ?? (i18n.resolvedLanguage ?? "en");
  const effectiveOs =
    os ??
    (typeof navigator !== "undefined" ? `${navigator.platform} ${navigator.userAgent}` : "unknown");

  const run = useCallback(() => {
    setBusy(true);
    setErr(null);
    api
      .diagExport()
      .then((path) => {
        setZipPath(path);
        onExported?.(path);
      })
      .catch((e: unknown) => {
        const human =
          e && typeof e === "object" && "human" in e
            ? String((e as { human: unknown }).human)
            : t("diag.failHint");
        setErr(human);
      })
      .finally(() => setBusy(false));
  }, [onExported, t]);

  const issueUrl = zipPath
    ? buildIssueUrl({
        version: __APP_VERSION__,
        os: effectiveOs,
        errorCode,
        symptom,
        diagFile: basename(zipPath),
        lang: effectiveLang.startsWith("zh") ? "zh-CN" : "en",
      })
    : null;

  return (
    <div className={compact ? "flex flex-col gap-2" : "flex flex-col gap-2.5"}>
      <button
        type="button"
        onClick={run}
        disabled={busy}
        className="rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green disabled:opacity-50"
      >
        {busy ? t("diag.exporting") : t("diag.export")}
      </button>

      {err && (
        <p className="font-mono text-[11px] leading-relaxed text-aibox-danger" role="alert">
          {t("diag.fail")}: {err}
        </p>
      )}

      {zipPath && (
        <div className="rounded-[4px] border border-aibox-ok/40 bg-aibox-ok/10 p-2.5">
          <p className="mb-1 break-all font-mono text-[11px] text-aibox-ok" data-testid="diag-zip">
            ✓ {zipPath}
          </p>
          <button
            type="button"
            onClick={() => window.open(issueUrl ?? "#", "_blank")}
            className="rounded-[4px] bg-aibox-info px-3 py-1.5 font-display text-xs font-semibold text-aibox-bg hover:opacity-90"
          >
            {t("diag.issue")}
          </button>
          <p className="mt-1.5 font-mono text-[10px] text-aibox-text2">{t("diag.issueNote")}</p>
        </div>
      )}
    </div>
  );
}
