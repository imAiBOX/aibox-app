/**
 * 前端侧密钥脱敏兜底（与 Rust `aibox_core::redact` 同口径）。
 *
 * **为什么前端也要做**：Rust 侧已在服务端把 key 脱敏，但错误串会经过很多路径
 * （服务商回显、库异常文案、未来新增的错误分支）。任何一处漏了，
 * 就会把 key 送到界面的「技术细节」里——用户截图/分享即泄露。
 * 这里做最后一道兜底：**宁可少显示信息，也不显示密钥**。
 *
 * 硬规则：key 只以 `sk-****1234`（保留末 4 位便于比对）形式出现。
 */

/** 形如 sk-xxx 的 key（保留末 4 位） */
const SK = /sk-[A-Za-z0-9][A-Za-z0-9._-]*/g;
/** Authorization: Bearer <token> */
const BEARER = /bearer\s+[A-Za-z0-9._-]+/gi;
/** URL 内联凭据 scheme://user:pass@host */
const URL_CRED = /:\/\/[^\s/@]+:[^\s/@]+@/g;
/** api_key = "xxxx" / "apiKey":"xxxx" */
const KV = /((?:api[_-]?key|apikey)["']?\s*[:=]\s*["']?)([A-Za-z0-9._-]{6,})(["']?)/gi;

/** 对任意文本做脱敏；不改长度语义，只替换敏感片段。 */
export function maskSecrets(text: string): string {
  if (!text) return text;
  let out = text;
  out = out.replace(SK, (m) => `sk-****${m.slice(-4)}`);
  out = out.replace(BEARER, "Bearer ****");
  out = out.replace(URL_CRED, "://****:****@");
  out = out.replace(KV, (_m, pre: string, _val: string, post: string) => `${pre}****${post}`);
  return out;
}
