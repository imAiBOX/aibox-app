/** CONFIG/VERIFY 共享纯类型（无运行时依赖，client 与 config 双向共用不产生环）。 */

/** 六路径 provider id（ConfigWriteInput.provider 冻结联合） */
export type ProviderId =
  | "openrouter"
  | "anthropic"
  | "openai"
  | "google"
  | "claude-oauth"
  | "ollama";

/** C4 推荐模型一条（registry modelProviders.*.recommendedModels） */
export interface ModelOption {
  id: string;
  name: string;
  note?: string;
  priceHint?: string;
}

/** registry modelProviders 单条（schema: aibox-registry/schemas/app.schema.json） */
export interface ProviderMeta {
  label?: string;
  authType?: string;
  keyFormat?: string;
  configTarget?: string;
  getKeyUrl?: string;
  recommendedModels?: ModelOption[];
  notes?: string;
}
