/**
 * 前端脱敏兜底测试（独立审计 2026-11-20 REJECT-1）。
 *
 * 任何 key 出现在错误详情里 → 会随截图/分享泄露 → 必须进界面前就被掩掉。
 */
import { describe, expect, it } from "vitest";
import { maskSecrets } from "./redact";

describe("maskSecrets", () => {
  it("sk- 开头的 key → sk-****1234（保留末 4 位）", () => {
    expect(maskSecrets("bad key sk-ant-SECRET1234")).toBe("bad key sk-****1234");
    expect(maskSecrets("sk-or-v1-abcdef0123456789")).toBe("sk-****6789");
  });

  it("Bearer token / URL 内联凭据 / api_key= 都被掩掉", () => {
    expect(maskSecrets("Authorization: Bearer abcdef123456")).toContain("Bearer ****");
    expect(maskSecrets("x")).toBe("x");
    expect(maskSecrets("https://user:pass@example.com/x")).toBe("https://****:****@example.com/x");
    expect(maskSecrets("api_key=abcdef123456")).toBe("api_key=****");
    expect(maskSecrets('"apiKey":"abcdef123456"')).toBe('"apiKey":"****"');
  });

  it("不含敏感串的文本原样返回", () => {
    expect(maskSecrets("VERIFY_NET_FAIL: could not reach openrouter")).toBe(
      "VERIFY_NET_FAIL: could not reach openrouter",
    );
    expect(maskSecrets("")).toBe("");
  });
});
