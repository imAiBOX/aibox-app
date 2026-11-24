import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { api } from "../net/client";

const buttonClass = "shrink-0 rounded-[4px] border border-aibox-line px-3 py-1.5 font-mono text-xs text-aibox-text2 hover:border-aibox-green hover:text-aibox-green disabled:opacity-50";

export function PreservedPanel() {
  const { t } = useTranslation();
  const [names, setNames] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadFailed, setLoadFailed] = useState(false);
  const [revealFailed, setRevealFailed] = useState(false);
  const [revealing, setRevealing] = useState(false);
  const revealPending = useRef(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setLoadFailed(false);
    try {
      setNames(await api.managePreservedList());
    } catch {
      setNames([]);
      setLoadFailed(true);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function reveal(name: string) {
    if (revealPending.current) return;
    revealPending.current = true;
    setRevealing(true);
    setRevealFailed(false);
    try {
      await api.managePreservedReveal(name);
    } catch {
      setRevealFailed(true);
      await refresh();
    } finally {
      setRevealing(false);
      revealPending.current = false;
    }
  }

  return (
    <section className="panel space-y-3" aria-busy={loading || revealing}>
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div>
          <p className="font-display font-semibold text-aibox-cream">{t("s8.preservedTitle")}</p>
          <p className="mt-1 text-xs leading-relaxed text-aibox-text2">{t("s8.preservedLede")}</p>
        </div>
        <button type="button" className={buttonClass} disabled={loading || revealing} onClick={() => {
          setRevealFailed(false);
          void refresh();
        }}>
          {t("s8.restoreReload")}
        </button>
      </div>
      {loadFailed && <p role="alert" className="text-xs text-aibox-danger">{t("s8.preservedLoadFailed")}</p>}
      {revealFailed && <p role="alert" className="text-xs text-aibox-danger">{t("s8.preservedRevealFailed")}</p>}
      {!loading && !loadFailed && names.length === 0 && (
        <p className="text-xs text-aibox-text2">{t("s8.preservedEmpty")}</p>
      )}
      {names.length > 0 && (
        <ul className="space-y-2">
          {names.map((name) => (
            <li key={name} className="flex flex-wrap items-center justify-between gap-2 rounded border border-aibox-line p-2">
              <span className="break-all font-mono text-xs text-aibox-text2">{name}</span>
              <button
                type="button"
                className={buttonClass}
                aria-label={t("s8.preservedRevealNamed", { name })}
                disabled={loading || revealing}
                onClick={() => void reveal(name)}
              >
                {t("s8.preservedReveal")}
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
