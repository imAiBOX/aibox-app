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
    // 左侧是字母数字 / 下划线 / 连字符 / 点时都不算 key 起点（不是词边界）
    expect(maskSecrets("xsk-ant-SECRET1234")).toBe("xsk-ant-SECRET1234");
    expect(maskSecrets("_sk-ant-SECRET1234")).toBe("_sk-ant-SECRET1234");
    expect(maskSecrets("foo-sk-abcdefgh")).toBe("foo-sk-abcdefgh");
    expect(maskSecrets("pkg.sk-abcdefgh")).toBe("pkg.sk-abcdefgh");
  });

  it("真 key 前有分隔符时仍要掩（= : 等常见赋值符号）", () => {
    expect(maskSecrets("key=sk-ant-SECRET1234")).toBe("key=sk-****1234");
    expect(maskSecrets("(sk-ant-SECRET1234)")).toBe("(sk-****1234)");
  });

  it("漏网形态：base64 字符集 / Bearer / URL 凭据 / 多种 key 字段名（**完整输出断言**）", () => {
    expect(maskSecrets("token sk-abc+/=defgh1234")).toBe("token sk-****1234");
    // 完整断言：不能只断言 contains（后面残留也可能通过）
    expect(maskSecrets("Authorization: Bearer abcdef123456")).toBe("Authorization: Bearer ****");
    expect(maskSecrets("https://user:pass@example.com/x")).toBe("https://****:****@example.com/x");
    expect(maskSecrets("api_key=abcdef123456")).toBe("api_key=****");
    expect(maskSecrets('"apiKey":"abcdef123456"')).toBe('"apiKey":"****"');
    expect(maskSecrets("secret_key=abcdef123456")).toBe("secret_key=****");
    expect(maskSecrets("access_token=abcdef123456")).toBe("access_token=****");
    expect(maskSecrets("auth_token=abcdef123456")).toBe("auth_token=****");
    expect(maskSecrets("authorization=abcdef123456")).toBe("authorization=****");
  });

  it("认证方案凭据整段掩掉（Basic / Digest / 含 ~ : 的 token）", () => {
    // Basic 的 base64 凭据也必须掩掉（不能只掩方案词）
    expect(maskSecrets("authorization=Basic dXNlcjpwYXNz")).toBe("authorization=Basic ****");
    expect(maskSecrets("Authorization: Basic dXNlcjpwYXNz")).toBe("Authorization: Basic ****");
    // Bearer 后面带 ~ : 等字符 → 整段掩掉，不能残留半截
    expect(maskSecrets("Authorization: Bearer abcdef~PRIVATE")).toBe("Authorization: Bearer ****");
    expect(maskSecrets("Authorization: Bearer abc:def~ghi")).toBe("Authorization: Bearer ****");
    // Digest 的挑战参数
    expect(maskSecrets('digest response="deadbeef", nonce="xyz"')).toBe(
      'digest response="****", nonce="****"',
    );
  });

  it("不含敏感串的文本原样返回", () => {
    expect(maskSecrets("VERIFY_NET_FAIL: could not reach openrouter")).toBe(
      "VERIFY_NET_FAIL: could not reach openrouter",
    );
    expect(maskSecrets("")).toBe("");
  });
});
