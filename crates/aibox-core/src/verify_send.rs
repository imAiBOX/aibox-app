//! VERIFY：经 provider 发 1 条**真实请求**（max_tokens=8 的 "Say OK"），
//! 必须真实收到模型文本才算通；网络失败 → 结构化错误，不接受「无报错」弱判定。
//!
//! 端点规则（baseUrl 缺省用官方默认值；给定则作为 API 根拼接）：
//! - openrouter/openai/ollama → `{base}/chat/completions`（OpenAI 兼容）
//! - anthropic → `{base}/messages`（x-api-key + anthropic-version 头）
//! - google → `{base}/models/{model}:generateContent?key=...`
//! - claude-oauth → 由 `openclaw onboard` 完成，aibox 不代发 → 结构化错误

use serde::Serialize;
use serde_json::{json, Value};

use crate::err::{CoreError, CoreResult};

/// VerifyInput。
#[derive(Debug, Clone, Default)]
pub struct VerifyInput {
    pub provider: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub base_url: Option<String>,
}

/// VerifyResult：`{ok, text?, error?}`。
#[derive(Debug, Clone, Serialize, Default)]
pub struct VerifyResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 构造好的请求。
#[derive(Debug, Clone)]
pub struct VerifyRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

const DEFAULT_MODEL: &str = "claude-sonnet-4-5";
const DEFAULT_MODELS: &[(&str, &str)] = &[
    ("openrouter", "anthropic/claude-sonnet-4"),
    ("openai", "gpt-5.1"),
    ("google", "gemini-2.5-flash"),
    ("ollama", "qwen2.5:7b"),
];

fn default_model(provider: &str) -> &'static str {
    DEFAULT_MODELS
        .iter()
        .find(|(p, _)| *p == provider)
        .map(|(_, m)| *m)
        .unwrap_or(DEFAULT_MODEL)
}

fn trim_slash(s: &str) -> &str {
    s.trim().trim_end_matches('/')
}

/// 生成请求（纯函数，单测锁端点/头/体）。
pub fn build_request(input: &VerifyInput) -> CoreResult<VerifyRequest> {
    let provider = input.provider.trim().to_ascii_lowercase();
    let model = input
        .model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| default_model(&provider))
        .to_string();
    let key = input.api_key.as_deref().filter(|k| !k.trim().is_empty());
    let prompt = "Say OK";
    let max_tokens = 8;

    match provider.as_str() {
        "claude-oauth" => Err(CoreError::new(
            "VERIFY_UNSUPPORTED_PROVIDER",
            "Claude subscription (OAuth) is verified by `openclaw onboard` — aibox does not send this request itself",
        )),
        "openrouter" | "openai" | "ollama" => {
            let base = match input.base_url.as_deref().filter(|b| !b.trim().is_empty()) {
                Some(b) => trim_slash(b).to_string(),
                None if provider == "openrouter" => "https://openrouter.ai/api/v1".into(),
                None if provider == "openai" => "https://api.openai.com/v1".into(),
                _ => "http://127.0.0.1:11434/v1".into(),
            };
            if provider != "ollama" && key.is_none() {
                return Err(CoreError::new(
                    "VERIFY_NO_KEY",
                    format!("Missing apiKey for provider {provider}"),
                ));
            }
            let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
            if let Some(k) = key {
                headers.push(("Authorization".to_string(), format!("Bearer {k}")));
            }
            let body = json!({
                "model": model,
                "max_tokens": max_tokens,
                "messages": [{"role": "user", "content": prompt}]
            });
            Ok(VerifyRequest {
                url: format!("{base}/chat/completions"),
                headers,
                body: body.to_string(),
            })
        }
        "anthropic" => {
            let base = match input.base_url.as_deref().filter(|b| !b.trim().is_empty()) {
                Some(b) => trim_slash(b).to_string(),
                None => "https://api.anthropic.com/v1".into(),
            };
            let k = key.ok_or_else(|| {
                CoreError::new("VERIFY_NO_KEY", "Missing apiKey for provider anthropic")
            })?;
            Ok(VerifyRequest {
                url: format!("{base}/messages"),
                headers: vec![
                    ("Content-Type".to_string(), "application/json".to_string()),
                    ("x-api-key".to_string(), k.to_string()),
                    ("anthropic-version".to_string(), "2023-06-01".to_string()),
                ],
                body: json!({
                    "model": model,
                    "max_tokens": max_tokens,
                    "messages": [{"role": "user", "content": prompt}]
                })
                .to_string(),
            })
        }
        "google" => {
            let base = match input.base_url.as_deref().filter(|b| !b.trim().is_empty()) {
                Some(b) => trim_slash(b).to_string(),
                None => "https://generativelanguage.googleapis.com/v1beta".into(),
            };
            let k = key.ok_or_else(|| {
                CoreError::new("VERIFY_NO_KEY", "Missing apiKey for provider google")
            })?;
            Ok(VerifyRequest {
                url: format!("{base}/models/{model}:generateContent?key={k}"),
                headers: vec![("Content-Type".to_string(), "application/json".to_string())],
                body: json!({
                    "contents": [{"parts": [{"text": prompt}]}],
                    "generationConfig": {"maxOutputTokens": max_tokens}
                })
                .to_string(),
            })
        }
        other => Err(CoreError::new(
            "VERIFY_UNSUPPORTED_PROVIDER",
            format!("Unknown provider: {other}"),
        )),
    }
}

/// 从 2xx 响应体提取模型文本（纯函数）。
pub fn parse_response(provider: &str, body: &str) -> CoreResult<String> {
    let v: Value = serde_json::from_str(body)
        .map_err(|e| CoreError::new("VERIFY_BAD_RESPONSE", format!("Response is not JSON: {e}")))?;
    let text = match provider {
        "anthropic" => v
            .pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .map(String::from),
        "google" => v
            .pointer("/candidates/0/content/parts/0/text")
            .and_then(|t| t.as_str())
            .map(String::from),
        // OpenAI 兼容族
        _ => v
            .pointer("/choices/0/message/content")
            .and_then(|t| t.as_str())
            .map(String::from),
    };
    match text {
        Some(t) if !t.trim().is_empty() => Ok(t.trim().to_string()),
        Some(_) => Err(CoreError::new(
            "VERIFY_EMPTY_TEXT",
            "Provider responded but with empty text — not accepted as success",
        )),
        None => Err(CoreError::new(
            "VERIFY_BAD_RESPONSE",
            format!(
                "Provider response missing model text: {}",
                crate::exec::truncate_utf8(body, 300).replace('\n', " ")
            ),
        )),
    }
}

/// 非 2xx → 结构化错误（保留 provider 错误摘要，key 已不在 URL 之外出现）。
pub fn http_error(provider: &str, status: u16, body: &str) -> CoreError {
    let raw = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            v.pointer("/error/message")
                .or_else(|| v.pointer("/message"))
                .and_then(|m| m.as_str())
                .map(String::from)
        })
        .unwrap_or_else(|| crate::exec::truncate_utf8(body, 200).replace('\n', " "));
    // 服务端可能回显 key → 强制脱敏
    let (msg, _) = crate::redact::scan_and_mask(&raw);
    CoreError::new(
        format!("VERIFY_HTTP_{status}"),
        format!("{provider} rejected the test request (HTTP {status}): {msg}"),
    )
}

/// 执行真实验证（同步；调用方放入 spawn_blocking）。
/// 网络失败 → `ok:false` + `error: "VERIFY_NET_FAIL: ..."`（结构化，绝不假成功）。
pub fn verify_send(input: &VerifyInput) -> VerifyResult {
    let provider = input.provider.trim().to_ascii_lowercase();
    let req = match build_request(&VerifyInput {
        provider: provider.clone(),
        ..input.clone()
    }) {
        Ok(r) => r,
        Err(e) => {
            return VerifyResult {
                ok: false,
                text: None,
                error: Some(e.to_string()),
            }
        }
    };

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(30))
        .timeout_connect(std::time::Duration::from_secs(10))
        .user_agent("aibox-app/0.0.1")
        .build();

    let mut call = agent.post(&req.url);
    for (k, v) in &req.headers {
        call = call.set(k, v);
    }

    let response = match call.send_string(&req.body) {
        Ok(r) => r,
        Err(ureq::Error::Status(status, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            let e = http_error(&provider, status, &body);
            return VerifyResult {
                ok: false,
                text: None,
                error: Some(e.to_string()),
            };
        }
        Err(ureq::Error::Transport(t)) => {
            // transport 错误可能内嵌完整 URL（google 路由 key 在 query 里）→ 脱敏
            let (safe, _) = crate::redact::scan_and_mask(&t.to_string());
            return VerifyResult {
                ok: false,
                text: None,
                error: Some(
                    CoreError::new(
                        "VERIFY_NET_FAIL",
                        format!("Could not reach {provider}: {safe}"),
                    )
                    .to_string(),
                ),
            };
        }
    };

    let body = match response.into_string() {
        Ok(b) => b,
        Err(e) => {
            return VerifyResult {
                ok: false,
                text: None,
                error: Some(
                    CoreError::new("VERIFY_BAD_RESPONSE", format!("Read body failed: {e}"))
                        .to_string(),
                ),
            }
        }
    };

    match parse_response(&provider, &body) {
        Ok(text) => VerifyResult {
            ok: true,
            text: Some(text),
            error: None,
        },
        Err(e) => VerifyResult {
            ok: false,
            text: None,
            error: Some(e.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inp(provider: &str) -> VerifyInput {
        VerifyInput {
            provider: provider.into(),
            api_key: Some("sk-test-key-123".into()),
            model: None,
            base_url: None,
        }
    }

    #[test]
    fn endpoints_and_bodies_per_provider() {
        let r = build_request(&inp("openrouter")).unwrap();
        assert_eq!(r.url, "https://openrouter.ai/api/v1/chat/completions");
        assert!(r
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer sk-test-key-123"));
        let body: Value = serde_json::from_str(&r.body).unwrap();
        assert_eq!(body["max_tokens"], 8);
        assert_eq!(body["messages"][0]["content"], "Say OK");
        assert_eq!(body["model"], "anthropic/claude-sonnet-4", "default model");

        let r = build_request(&inp("anthropic")).unwrap();
        assert_eq!(r.url, "https://api.anthropic.com/v1/messages");
        assert!(r.headers.iter().any(|(k, _)| k == "x-api-key"));

        let r = build_request(&inp("google")).unwrap();
        assert!(r.url.contains("generativelanguage.googleapis.com"));
        assert!(r.url.contains(":generateContent?key=sk-test-key-123"));

        let r = build_request(&VerifyInput {
            provider: "ollama".into(),
            api_key: None,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(r.url, "http://127.0.0.1:11434/v1/chat/completions");
        assert!(!r.headers.iter().any(|(k, _)| k == "Authorization"));

        // baseUrl 覆盖
        let r = build_request(&VerifyInput {
            provider: "openai".into(),
            api_key: Some("k".into()),
            base_url: Some("https://proxy.example/v1/".into()),
            model: None,
        })
        .unwrap();
        assert_eq!(r.url, "https://proxy.example/v1/chat/completions");
    }

    #[test]
    fn missing_key_and_unknown_provider_are_structured() {
        let e = build_request(&VerifyInput {
            provider: "openai".into(),
            api_key: None,
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(e.code, "VERIFY_NO_KEY");
        let e = build_request(&VerifyInput {
            provider: "deepseek".into(),
            ..Default::default()
        })
        .unwrap_err();
        assert_eq!(e.code, "VERIFY_UNSUPPORTED_PROVIDER");
        let e = build_request(&inp("claude-oauth")).unwrap_err();
        assert_eq!(e.code, "VERIFY_UNSUPPORTED_PROVIDER");
    }

    #[test]
    fn parse_response_shapes() {
        let openai = r#"{"choices":[{"message":{"content":"OK"}}]}"#;
        assert_eq!(parse_response("openrouter", openai).unwrap(), "OK");
        let anthropic = r#"{"content":[{"type":"text","text":"OK"}]}"#;
        assert_eq!(parse_response("anthropic", anthropic).unwrap(), "OK");
        let google = r#"{"candidates":[{"content":{"parts":[{"text":"OK"}]}}]}"#;
        assert_eq!(parse_response("google", google).unwrap(), "OK");
        // 空文本 = 不算成功
        let empty = r#"{"choices":[{"message":{"content":""}}]}"#;
        assert_eq!(
            parse_response("openai", empty).unwrap_err().code,
            "VERIFY_EMPTY_TEXT"
        );
        // 缺字段
        let weird = r#"{"foo":1}"#;
        assert_eq!(
            parse_response("openai", weird).unwrap_err().code,
            "VERIFY_BAD_RESPONSE"
        );
        assert!(parse_response("openai", "not json").is_err());
    }

    #[test]
    fn http_error_carries_status_code() {
        let e = http_error("openai", 401, r#"{"error":{"message":"invalid api key"}}"#);
        assert_eq!(e.code, "VERIFY_HTTP_401");
        assert!(e.human.contains("invalid api key"));
        assert!(!e.human.contains("sk-test"), "不回显 key");
    }

    #[test]
    fn verify_send_local_unreachable_is_structured_not_success() {
        // 本地不存在的端口 → 必须 ok:false + VERIFY_NET_FAIL（绝不弱判定）
        let r = verify_send(&VerifyInput {
            provider: "ollama".into(),
            api_key: None,
            model: None,
            base_url: Some("http://127.0.0.1:1/v1".into()),
        });
        assert!(!r.ok);
        let err = r.error.expect("error must be present");
        assert!(err.contains("VERIFY_NET_FAIL"), "{err}");
    }
}
