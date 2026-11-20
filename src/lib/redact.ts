/**
 * 前端侧密钥脱敏兜底（与 Rust `aibox_core::redact` 同口径）。
 *
 * **为什么前端也要做**：Rust 侧已在服务端把 key 脱敏，但错误串会经过很多路径
 * （服务商回显、库异常文案、未来新增的错误分支）。任何一处漏了，
 * 就会把 key 送到界面的「技术细节」里——用户截图/分享即泄露。
 * 这里做最后一道兜底：**宁可少显示信息，也不显示密钥**。
 *
 * 硬规则：key 只以 `sk-****1234`（保留末 4 位便于比对）形式出现。
 *
 * **误伤防护**（独立审计 2026-11-20 REJECT）：`sk-` 前缀本身会撞上普通文案
 * （例如「sk-example 这种写法」）→ 因此要求：左侧不是字母数字/下划线，
 * 且 `sk-` 之后至少 8 位（真实 key 远长于此；短词不掩，避免把正常词改坏）。
 */

/**
 * sk- 开头的 key：左边界 + 至少 8 位后续字符 + 完整 token 字符集（含 base64 的 + / = ~ :）
 *
 * 左边界含 `-` 与 `.`：否则 `foo-sk-abcdefgh` 这类普通标识符会被误伤
 * （第三轮审计 NOTE）。右侧字符集放宽到 `~ :` 等常见 token 字符，避免漏掩。
 */
const SK = /(?<![A-Za-z0-9_.-])sk-[A-Za-z0-9][A-Za-z0-9._+/=~:-]{7,}/g;
/**
 * 认证方案 + 凭据（bearer / basic / digest）：
 * **整段凭据一起掩**，不能只掩方案词（否则 `Authorization: Basic <b64>` 的
 * base64 凭据会原样留在错误详情里 —— 第四轮审计 REJECT-1）。
 */
const AUTH = /\b(bearer|basic)\s+[A-Za-z0-9._+/=~:-]{4,}/gi;
/**
 * Digest 认证的挑战参数（response/nonce/…）—— 同样算凭据材料。
 * Digest 的凭据不是「方案名 + 一段 token」，而是参数列表 → 单独处理，
 * 不能套用上面的 AUTH 规则（否则会把参数名当 token 掩掉、真值反而留着）。
 */
const DIGEST_PARAM = /\b(response|nonce|cnonce|opaque)\s*=\s*"[^"]*"/gi;
/** URL 内联凭据 scheme://user:pass@host */
const URL_CRED = /:\/\/[^\s/@]+:[^\s/@]+@/g;
/** api_key = "xxxx" / "apiKey":"xxxx" / secret_key / access_token … */
/**
 * 字段名赋值形态：api_key=… / "apiKey":"…" / authorization=…
 *
 * 值用负向断言跳过 `Bearer` / `Basic` 这类**认证方案词**：
 * 否则 `Authorization: Bearer <token>` 会先被这条规则把 "Bearer" 当值掩掉，
 * 真正的 token 反而留在后面（第三轮审计发现的回归）。
 */
const KV = /((?:api[_-]?key|apikey|secret[_-]?key|access[_-]?token|auth[_-]?token|authorization)["']?\s*[:=]\s*["']?)(?!Bearer\b|Basic\b|Digest\b)([A-Za-z0-9._+/=~:-]{6,})(["']?)/gi;

/** 对任意文本做脱敏；不改长度语义，只替换敏感片段。 */
export function maskSecrets(text: string): string {
  if (!text) return text;
  let out = text;
  out = out.replace(SK, (m) => `sk-****${m.slice(-4)}`);
  out = out.replace(AUTH, (_m, scheme: string) => `${scheme} ****`);
  out = out.replace(DIGEST_PARAM, (_m, name: string) => `${name}="****"`);
  out = out.replace(URL_CRED, "://****:****@");
  out = out.replace(KV, (_m, pre: string, _val: string, post: string) => `${pre}****${post}`);
  return out;
}
