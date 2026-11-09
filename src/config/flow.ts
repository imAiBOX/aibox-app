/**
 * CONFIG 向导流程编排（C1-C5 / VERIFY）。
 * 纯 reducer（reduceWizard）+ 薄异步封装（runConfigWrite / runVerify），
 * 不依赖 React —— vitest 直接驱动 C1→C2→C3→C4→C5→VERIFY 全流转。
 *
 * key 安全：key 只存在于本流程内存态，不进 reducer 主状态、不进持久化、不进日志。
 */
import { api, type ConfigWriteInput, type VerifyInput } from "../net/client";
import { fallbackModels, providerById, validateKey, type KeyCheck, type ProviderPath } from "./providers";
import type { ModelOption, ProviderMeta } from "./types";

/** 向导步骤（S5 屏内子状态；与八态状态机正交） */
export type WizardStep = "C1" | "C2" | "C3" | "C4" | "C5" | "TODO";

export interface WizardState {
  step: WizardStep;
  providerId: string | null;
  /** C3 粘贴的 key（仅内存，绝不持久化） */
  key: string;
  /** C4 选中的模型 id */
  model: string | null;
  /** registry modelProviders 整包（keyFormat 真源 + recommendedModels）；未拉到 → 本地回落 */
  metas?: Record<string, ProviderMeta>;
  /** C5 写入中/结果 */
  writing: boolean;
  writeResult: { ok: boolean; backupPath: string } | null;
  /** 人话错误 i18n key（写入失败等） */
  errorKey: string | null;
}

export type WizardEvent =
  | { type: "PICK"; providerId: string } // C1 选路径（full→C2，todo→TODO）
  | { type: "BACK" } // 各步回退
  | { type: "NEXT" } // C2→C3、C3→C4（校验守卫）、C4→C5（守卫）
  | { type: "SET_KEY"; key: string } // C3 即时输入
  | { type: "SET_MODEL"; model: string } // C4 选模型
  | { type: "SET_META"; metas: Record<string, ProviderMeta> } // registry 数据注入
  | { type: "WRITE_START" }
  | { type: "WRITE_DONE"; ok: boolean; backupPath: string }
  | { type: "WRITE_FAIL"; errorKey: string }
  | { type: "RESET" };

export function createWizard(): WizardState {
  return {
    step: "C1",
    providerId: null,
    key: "",
    model: null,
    writing: false,
    writeResult: null,
    errorKey: null,
  };
}

function path(s: WizardState): ProviderPath | null {
  return providerById(s.providerId);
}

/** 当前 provider 的 registry 元数据（可空） */
export function wizardMeta(s: WizardState): ProviderMeta | undefined {
  return s.providerId ? s.metas?.[s.providerId] : undefined;
}

/** C4 模型清单：registry recommendedModels 优先，本地 fallbackModels 兑底 */
export function wizardModels(s: WizardState): ModelOption[] {
  const p = path(s);
  if (!p) return [];
  return wizardMeta(s)?.recommendedModels ?? fallbackModels(p);
}

/** C3 当前校验结果（registry keyFormat 优先，回落本地 pattern） */
export function wizardKeyCheck(s: WizardState): KeyCheck | null {
  const p = path(s);
  if (!p || p.noKey) return null;
  return validateKey(p, s.key, wizardMeta(s)?.keyFormat);
}

/** 向导纯转移（不发请求；C5 异步结果经 WRITE_* 事件回注） */
export function reduceWizard(s: WizardState, e: WizardEvent): WizardState {
  switch (e.type) {
    case "PICK": {
      const p = providerById(e.providerId);
      if (!p) return s;
      // 换路径 = 重置 key/模型/写入态（防上一家的 key 混进下一家）
      const fresh: WizardState = {
        ...createWizard(),
        providerId: p.id,
        step: p.status === "full" ? "C2" : "TODO",
      };
      return { ...fresh, metas: s.metas };
    }
    case "BACK":
      switch (s.step) {
        case "C2":
          return { ...s, step: "C1" };
        case "C3":
          return { ...s, step: "C2" };
        case "C4":
          return { ...s, step: "C3" };
        case "C5":
          return { ...s, step: "C4", writeResult: null, errorKey: null };
        case "TODO":
          return { ...s, step: "C1" };
        default:
          return s;
      }
    case "NEXT":
      switch (s.step) {
        case "C2":
          return { ...s, step: "C3" };
        case "C3": {
          // 守卫：key 格式不过关不放行（无 key 路线直通）
          const p = path(s);
          if (!p) return s;
          if (p.noKey) return { ...s, step: "C4" };
          return wizardKeyCheck(s)?.ok ? { ...s, step: "C4" } : s;
        }
        case "C4":
          // 守卫：必须选了模型
          return s.model ? { ...s, step: "C5" } : s;
        default:
          return s;
      }
    case "SET_KEY":
      // 即时校验只反馈不清空；key 只进本流程态
      return { ...s, key: e.key, errorKey: null };
    case "SET_MODEL":
      return { ...s, model: e.model };
    case "SET_META":
      return { ...s, metas: e.metas };
    case "WRITE_START":
      return { ...s, writing: true, errorKey: null, writeResult: null };
    case "WRITE_DONE":
      return {
        ...s,
        writing: false,
        writeResult: e.ok ? { ok: true, backupPath: e.backupPath } : null,
        errorKey: e.ok ? null : "s5.writeFail",
      };
    case "WRITE_FAIL":
      return { ...s, writing: false, errorKey: e.errorKey, writeResult: null };
    case "RESET":
      return createWizard();
    default: {
      const _never: never = e;
      void _never;
      return s;
    }
  }
}

/* ── 异步薄封装（真 invoke / mock 由 client 单一入口决定） ── */

export interface WriteOutcome {
  ok: boolean;
  backupPath: string;
  errorKey?: string;
}

/** C5 写入：备份→合并→回读（Rust 侧硬规则）；失败给大人话 errorKey */
export async function runConfigWrite(input: ConfigWriteInput): Promise<WriteOutcome> {
  try {
    const r = await api.configWrite(input);
    if (r.ok && r.backupPath) return { ok: true, backupPath: r.backupPath };
    return { ok: false, backupPath: "", errorKey: "s5.writeFail" };
  } catch {
    return { ok: false, backupPath: "", errorKey: "s5.writeFail" };
  }
}

export interface VerifyOutcome {
  ok: boolean;
  text?: string;
  /** 人话错误 key（弱判定拒绝也走这里） */
  errorKey?: string;
  /** 原始错误细节（小字展示，不含 key） */
  detail?: string;
  latencyMs?: number;
}

/** 错误 → 人话 i18n key（不给用户抛英文栈） */
export function humanVerifyError(error?: string): string {
  const e = (error ?? "").toLowerCase();
  if (/401|403|unauthorized|invalid|revoke|wrong/.test(e)) return "s6.errKey";
  if (/network|timeout|timed out|unreachable|connect|dns|offline|proxy/.test(e)) {
    return "s6.errNet";
  }
  return "s6.failGeneric";
}

/**
 * VERIFY：发一条真实请求（max_tokens=8 "Say OK"）。
 * **弱判定拒绝**：ok 但没收到非空文本 → 一律不算过（硬规则）。
 */
export async function runVerify(input: VerifyInput): Promise<VerifyOutcome> {
  const started = Date.now();
  try {
    const r = await api.verifySend(input);
    const latencyMs = Date.now() - started;
    if (r.ok && r.text && r.text.trim().length > 0) {
      return { ok: true, text: r.text, latencyMs };
    }
    if (r.ok) {
      // 弱判定拒绝：无文本/空白文本
      return { ok: false, errorKey: "s6.errWeak", latencyMs };
    }
    return { ok: false, errorKey: humanVerifyError(r.error), detail: r.error, latencyMs };
  } catch (err) {
    return {
      ok: false,
      errorKey: humanVerifyError(err instanceof Error ? err.message : String(err)),
      latencyMs: Date.now() - started,
    };
  }
}
