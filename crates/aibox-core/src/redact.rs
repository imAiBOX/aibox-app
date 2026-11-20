//! key 脱敏与诊断导出前的强制正则扫描（硬规则）。
//!
//! - 任何返回/日志中的 key 一律 `sk-****1234` 形式（保留末 4 位，便于用户比对）。
//! - 导出诊断包前对**全部文本**跑正则 `(anthropic|openai|sk-|api[_-]?key)` 等模式，
//!   命中 → 替换为 [`MASK`] 并计入报告。

use regex::Regex;
use std::sync::OnceLock;

/// 脱敏占位（`sk-****1234`）。
pub const MASK: &str = "sk-****1234";

/// 诊断扫描模式（anthropic|openai|sk-|api[_-]?key 等）。
/// 额外两条防御：`Bearer <token>`、URL 内联凭据 `://user:pass@`。
pub fn patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            Regex::new(r"(?i)anthropic").unwrap(),
            Regex::new(r"(?i)openai").unwrap(),
            Regex::new(r"(?i)sk-[A-Za-z0-9][A-Za-z0-9._-]*").unwrap(),
            Regex::new(r"(?i)api[_-]?key").unwrap(),
            // 认证方案与凭据（与前端 src/lib/redact.ts 同口径 —— 两侧规则必须一致，
            // 否则同一段文本在服务端与界面显示不同）：
            // - Authorization 头整段（含 scheme 与凭据；凭据里可能有 , ; 分隔符）
            // - 非头部的 bearer/basic 凭据
            // - Digest 挑战参数（带引号与不带引号两种形式）
            Regex::new(r"(?i)\b(?:proxy-)?authorization\s*[:=]\s*[^\r\n]+").unwrap(),
            Regex::new(r"(?i)\b(?:bearer|basic)\s+[A-Za-z0-9._+/=~:;,]{4,}").unwrap(),
            Regex::new(r#"(?i)\b(?:response|nonce|cnonce|opaque)\s*=\s*(?:"[^"]*"|'[^']*'|[^\s,;]+)"#).unwrap(),
            Regex::new(r"://[^\s/@]+:[^\s/@]+@").unwrap(),
        ]
    })
}

/// 对文本跑全部扫描模式：命中处替换为 [`MASK`]，返回（脱敏文本, 命中总次数）。
pub fn scan_and_mask(text: &str) -> (String, usize) {
    let mut out = text.to_string();
    let mut hits = 0usize;
    for re in patterns() {
        let n = re.find_iter(&out).count();
        if n > 0 {
            hits += n;
            out = re.replace_all(&out, MASK).into_owned();
        }
    }
    (out, hits)
}

/// 只扫描不替换：返回命中总次数。
pub fn count_hits(text: &str) -> usize {
    patterns().iter().map(|re| re.find_iter(text).count()).sum()
}

/// 单个 key 值的脱敏（C5：任何返回/日志中出现的 key）。
/// 保留 `sk-` 前缀与末 4 位：`sk-ant-abcdef1234` → `sk-****1234`。
pub fn mask_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.len() <= 8 {
        return MASK.to_string();
    }
    let tail: String = trimmed.chars().rev().take(4).collect();
    let tail: String = tail.chars().rev().collect();
    format!("sk-****{tail}")
}

/// 对结构化 JSON 值做全量扫描+脱敏（返回新值与命中数）。
pub fn scan_value(v: &serde_json::Value) -> (serde_json::Value, usize) {
    let text = v.to_string();
    let (masked, hits) = scan_and_mask(&text);
    if hits == 0 {
        return (v.clone(), 0);
    }
    // 脱敏后的文本可能不再是合法 JSON（值里被塞了占位符），解析失败则原样返回文本形态
    match serde_json::from_str(&masked) {
        Ok(parsed) => (parsed, hits),
        Err(_) => (serde_json::Value::String(masked), hits),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_masks_sk_key() {
        let text = "token=sk-ant-api03-abcdefXYZ1234567890 stored";
        let (masked, hits) = scan_and_mask(text);
        assert!(hits >= 1, "must hit sk- pattern");
        assert!(
            !masked.contains("abcdefXYZ1234567890"),
            "raw key must not survive"
        );
        assert!(masked.contains(MASK));
        assert!(masked.contains("stored"));
    }

    #[test]
    fn scan_masks_provider_words_and_apikey_labels() {
        let text = "provider=anthropic endpoint=openai.com my_api_key=abc";
        let (masked, hits) = scan_and_mask(text);
        assert!(hits >= 3, "hits={hits}");
        assert!(!masked.contains("anthropic"));
        assert!(!masked.contains("openai"));
        assert!(!masked.contains("api_key"));
    }

    #[test]
    fn mask_key_keeps_last_four() {
        assert_eq!(mask_key("sk-or-v1-abcdefghijklmnop1234"), "sk-****1234");
        assert_eq!(mask_key("short"), MASK);
        assert_eq!(mask_key("  sk-or-v1-abcdefghijklmnop9999  "), "sk-****9999");
    }

    #[test]
    fn count_hits_works() {
        assert_eq!(count_hits("nothing here"), 0);
        assert!(count_hits("sk-1 and API-KEY") >= 2);
    }
}

#[cfg(test)]
mod auth_tests {
    use super::*;

    /// 认证凭据的脱敏口径（与前端 src/lib/redact.ts 一致 —— 两侧必须同口径）。
    #[test]
    fn auth_credentials_are_masked() {
        let cases = [
            ("Authorization: Bearer abcdef123456", "abcdef123456"),
            ("Authorization: Basic dXNlcjpwYXNz", "dXNlcjpwYXNz"),
            // 凭据里带分隔符 → 必须整段掩掉，不能只掩前半截
            ("Authorization: Bearer abcdef,PRIVATE", "PRIVATE"),
            ("proxy-authorization: bearer abcdef123456", "abcdef123456"),
            // Digest 参数（带引号与不带引号）
            ("digest response=\"deadbeef\", nonce=\"xyz\"", "deadbeef"),
            ("digest response=deadbeef, nonce=xyz", "deadbeef"),
        ];
        for (input, secret) in cases {
            let (out, hits) = scan_and_mask(input);
            assert!(hits > 0, "未命中脱敏: {input}");
            assert!(!out.contains(secret), "凭据残留: {input} -> {out}");
        }
    }
}
