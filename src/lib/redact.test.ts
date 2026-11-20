/**
 * 前端脱敏兜底测试（独立审计 2026-11-20 REJECT-1 + 复核 REJECT：误伤与漏网）。
 *
 * 任何 key 出现在错误详情里 → 会随截图/分享泄露 → 必须进界面前就被掩掉。
 * 同时要**不误伤**普通文案（把「sk-example 这种写法」改坏同样是缺陷）。
 */
import { describe, expect, it } from "vitest";
import { maskSecrets } from "./redact";

describe("maskSecrets", () => {
  it("sk- 开头的 key → sk-****1234（保留末 4 位）", () => {
    expect(maskSecrets("bad key sk-ant-SECRET1234")).toBe("bad key sk-****1234");
    expect(maskSecrets("sk-or-v1-abcdef0123456789")).toBe("sk-****6789");
  });

  it("不误伤普通文案（复核 REJECT）", () => {
    // 短词 / 非 key 形态 → 原样保留
    expect(maskSecrets("sk-example 这种写法")).toBe("sk-example 这种写法");
    expect(maskSecrets("task-1")).toBe("task-1");
    expect(maskSecrets("risk")).toBe("risk");
    // 左侧是字母数字时不算 key（不是词边界）
    expect(maskSecrets("xsk-ant-SECRET1234")).toBe("xsk-ant-SECRET1234");
  });

  it("漏网形态：base64 字符集 / Bearer / URL 凭据 / 多种 key 字段名", () => {
    expect(maskSecrets("token sk-abc+/=defgh1234")).toBe("token sk-****1234");
    expect(maskSecrets("Authorization: Bearer abcdef123456")).toContain("Bearer ****");
    expect(maskSecrets("https://user:pass@example.com/x")).toBe("https://****:****@example.com/x");
    expect(maskSecrets("api_key=abcdef123456")).toBe("api_key=****");
    expect(maskSecrets('"apiKey":"abcdef123456"')).toBe('"apiKey":"****"');
    expect(maskSecrets("secret_key=abcdef123456")).toBe("secret_key=****");
    expect(maskSecrets("access_token=abcdef123456")).toBe("access_token=****");
  });

  it("不含敏感串的文本原样返回", () => {
    expect(maskSecrets("VERIFY_NET_FAIL: could not reach openrouter")).toBe(
      "VERIFY_NET_FAIL: could not reach openrouter",
    );
    expect(maskSecrets("")).toBe("");
  });
});
