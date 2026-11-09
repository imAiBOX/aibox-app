/**
 * 诊断导出 → GitHub Issue 预填 URL：人话模板（版本/OS/错误码）+ 敏感字段脱敏。
 */
import { describe, expect, it } from "vitest";
import { buildIssueUrl, redact, ISSUE_NEW_URL } from "./issue";

const base = {
  version: "0.0.1",
  os: "Windows 11 x64",
  errorCode: "NET_ALL_CHANNELS_DEAD",
  symptom: "所有下载源超时",
  diagFile: "aibox-diagnostic-20261109.zip",
};

describe("redact（前端二次脱敏）", () => {
  it("sk- token → sk-****", () => {
    expect(redact("key=sk-abc123XYZ")).not.toContain("abc123XYZ");
    expect(redact("sk-abc123XYZ")).toContain("sk-****");
  });

  it("api_key 赋值 → [redacted]", () => {
    expect(redact('api_key: "supersecret99"')).not.toContain("supersecret99");
    expect(redact("apiKey=verysecret")).not.toContain("verysecret");
  });

  it("provider 名命中也替换", () => {
    expect(redact("anthropic token")).toBe("[redacted] token");
  });

  it("普通文本原样", () => {
    expect(redact("All download sources timed out")).toBe(
      "All download sources timed out",
    );
  });
});

describe("buildIssueUrl", () => {
  it("指向 issues/new 且 title/body 已编码", () => {
    const url = buildIssueUrl(base);
    expect(url.startsWith(`${ISSUE_NEW_URL}?title=`)).toBe(true);
    expect(url).toContain("body=");
    expect(url).not.toContain("\n"); // 换行必须被 encodeURIComponent
    const body = decodeURIComponent(url.split("body=")[1]);
    expect(body).toContain("0.0.1"); // 版本
    expect(body).toContain("Windows 11 x64"); // OS
    expect(body).toContain("NET_ALL_CHANNELS_DEAD"); // 错误码
    expect(body).toContain("aibox-diagnostic-20261109.zip"); // 诊断包
  });

  it("不含任何 key（即使上下文被污染也过一遍脱敏）", () => {
    const dirty = { ...base, symptom: "failed with sk-leakedKEY123456 and api_key=zzztopsecret" };
    const url = buildIssueUrl(dirty);
    expect(url).not.toContain("leakedKEY123456");
    expect(url).not.toContain("zzztopsecret");
  });

  it("中文模板（lang=zh-CN）", () => {
    const url = buildIssueUrl({ ...base, lang: "zh-CN" });
    const body = decodeURIComponent(url.split("body=")[1]);
    expect(body).toContain("发生了什么");
    expect(body).toContain("版本：");
    expect(decodeURIComponent(url.split("title=")[1].split("&")[0])).toContain("【AiBOX 安装问题】");
  });

  it("无 errorCode 也有兜底", () => {
    const url = buildIssueUrl({ ...base, errorCode: undefined, diagFile: undefined });
    const body = decodeURIComponent(url.split("body=")[1]);
    expect(body).toContain("unknown");
    expect(body).toContain("not exported");
  });

  it("title 超长截断 100", () => {
    const url = buildIssueUrl({ ...base, errorCode: "X".repeat(300) });
    const title = decodeURIComponent(url.split("title=")[1].split("&")[0]);
    expect(title.length).toBeLessThanOrEqual(100);
  });
});
