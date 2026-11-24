/**
 * 批量「一个 key 通吃」——**纯逻辑**（可单测；UI 在 components/KeyBatchPanel.tsx）。
 *
 * 为什么单独抽出来：批量写入要处理三件事，逻辑不能埋在组件里：
 * 1. **哪些应用能自动写**（同向导和 MCP 的平台配置规则）
 * 2. **逐个写入、逐个回执**（一个失败不能影响其他；每个应用独立备份）
 * 3. **如实呈现失败原因**（不可写 / 写入失败 / 找不到条目 → 三种不同的话）
 */

import { configCapability } from "./config-capability-v1";

/** 一个候选应用（已装 + registry 元数据）。 */
export interface BatchCandidate {
  appId: string;
  name: string;
  /** 该应用对这个 provider 能否自动写入 */
  writable: boolean;
  /** 不可写的原因码（给人看的文案在 UI 层映射） */
  reason?: "notSupported" | "unknown";
}

/** 批量写入的一条回执（逐应用）。 */
export interface BatchReceipt {
  appId: string;
  name: string;
  ok: boolean;
  /** 人话错误键（i18n key） */
  errorKey?: string;
  /** 机器可读错误码 */
  errorCode?: string;
  /** 成功时的备份路径（用户可回滚） */
  backupPath?: string;
}

/** 把「已装列表 + registry 元数据」整理成候选列表（可写排前面）。 */
export function buildCandidates(
  installed: { appId: string; name?: string }[],
  profiles: Record<string, unknown>,
  provider: string,
  platform: string | undefined,
): BatchCandidate[] {
  const list = installed.map((it) => {
    const capability = configCapability(profiles[it.appId], it.appId, provider, platform);
    const writable = capability === "assisted";
    const reason: BatchCandidate["reason"] = capability === "unavailable" ? "unknown" : capability === "manual" ? "notSupported" : undefined;
    return {
      appId: it.appId,
      name: it.name ?? it.appId,
      writable,
      ...(reason ? { reason } : {}),
    };
  });
  return list.sort((a, b) => Number(b.writable) - Number(a.writable) || a.appId.localeCompare(b.appId));
}

/** 只保留被勾选且可写的应用（不可写的**不写**，也不假装写）。 */
export function writableSelected(candidates: BatchCandidate[], selected: string[]): BatchCandidate[] {
  const set = new Set(selected);
  return candidates.filter((c) => c.writable && set.has(c.appId));
}

/** 批量结果汇总。 */
export function summarize(receipts: BatchReceipt[]): { ok: number; failed: number; total: number } {
  const ok = receipts.filter((r) => r.ok).length;
  return { ok, failed: receipts.length - ok, total: receipts.length };
}

/** key 是否符合该 provider 的格式（registry `keyFormat` 正则；缺席 → 只要求非空）。 */
export function keyOk(key: string, keyFormat?: string): boolean {
  const k = key.trim();
  if (!k) return false;
  if (!keyFormat) return true;
  try {
    return new RegExp(keyFormat).test(k);
  } catch {
    return true; // 正则坏掉 → 不拦用户（后端仍会校验）
  }
}
