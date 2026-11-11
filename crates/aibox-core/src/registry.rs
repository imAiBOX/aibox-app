//! registry 多源拉取与 12h 缓存（S2：jsdelivr / raw / GitHub API / aibox.im **任二一致**才采信）。
//!
//! - 缓存：`~/.aibox/cache/registry/<name>`，mtime 12h 内直接命中（`force=true` 绕过）
//! - 0~1 个源可达 → 退回缓存（可用性）；无缓存 → `REGISTRY_SINGLE_SOURCE`/`REGISTRY_UNAVAILABLE`
//! - **唯一成功源恰为 GitHub API** → 采信（contents API 正文先过 git blob sha1 完整性校验）
//! - ≥2 个源可达但互不一致 → `REGISTRY_MISMATCH`（**不采信**，防篡改）
//! - locales 也走同一套多源+缓存

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::err::{io_err, CoreError, CoreResult};
use crate::paths;

pub const OWNER: &str = "imAiBOX";
pub const REPO: &str = "aibox-registry";
pub const BRANCH: &str = "main";
/// 缓存有效期（12h mtime）。
pub const CACHE_TTL: Duration = Duration::from_secs(12 * 3600);
/// 单源超时。
const SOURCE_TIMEOUT: Duration = Duration::from_secs(8);
const USER_AGENT: &str = "aibox-app/0.0.1";

/// 四源 URL（顺序：jsdelivr → raw → GitHub API → aibox.im /meta 镜像）。
///
/// **路径约定（两端必须一致）**：aibox.im 侧是 registry 的**同构镜像**，即
/// `https://aibox.im/meta/<rel>` ↔ registry `<rel>`（如 `index.json`、
/// `apps/<id>/app.json`、`locales/<lang>.json`）。镜像由官网构建脚本
/// `aibox-site/scripts/gen-meta.mjs` 输出到 `dist/meta/<rel>`，改路径需两端同步。
pub fn source_urls(rel: &str) -> Vec<(&'static str, String)> {
    let rel = rel.trim_start_matches('/');
    vec![
        (
            "jsdelivr",
            format!("https://cdn.jsdelivr.net/gh/{OWNER}/{REPO}@{BRANCH}/{rel}"),
        ),
        (
            "raw",
            format!("https://raw.githubusercontent.com/{OWNER}/{REPO}/{BRANCH}/{rel}"),
        ),
        (
            "github-api",
            format!("https://api.github.com/repos/{OWNER}/{REPO}/contents/{rel}?ref={BRANCH}"),
        ),
        ("aibox-im", format!("https://aibox.im/meta/{rel}")),
    ]
}

/// 单次 GET（超时 8s，跟随重定向；GitHub API 带 UA + JSON Accept）。
fn http_get(url: &str, source: &str) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(SOURCE_TIMEOUT)
        .timeout_connect(SOURCE_TIMEOUT)
        .user_agent(USER_AGENT)
        .build();
    let mut req = agent.get(url).set("User-Agent", USER_AGENT);
    if source == "github-api" {
        // JSON envelope（含 base64 content + blob sha）——不做 raw，拿得到完整性凭据
        req = req.set("Accept", "application/vnd.github+json");
    }
    match req.call() {
        Ok(resp) => resp.into_string().map_err(|e| format!("read body: {e}")),
        Err(ureq::Error::Status(code, r)) => {
            let msg = r.into_string().unwrap_or_default();
            Err(format!("HTTP {code}: {}", truncate(&msg, 120)))
        }
        Err(ureq::Error::Transport(t)) => Err(t.to_string()),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut end = n;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// git blob 对象哈希 = `sha1("blob {len}\0{content}")`（Git 全网同款完整性机制）。
pub fn git_blob_sha1(content: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", content.len()).as_bytes());
    h.update(content);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// 标准 base64 解码（容忍 GitHub contents API 的 `\r\n` 折行与空白）。
fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rev = [0xffu8; 256];
    for (i, c) in TABLE.iter().enumerate() {
        rev[*c as usize] = i as u8;
    }
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in input.bytes() {
        if c == b'=' || c == b'\r' || c == b'\n' || c == b' ' || c == b'\t' {
            continue;
        }
        let v = rev[c as usize];
        if v == 0xff {
            return Err(format!("base64: invalid byte 0x{c:02x}"));
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

/// GitHub contents API JSON envelope → 原文，并做完整性校验：
/// base64 解码 `content` 后计算 git blob sha1，与响应 `sha` 字段比对，一致才采信。
/// （防 API 层/中间人篡改；不一致或解析失败 → 走既有错误路径。）
pub fn decode_github_contents(raw: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| format!("api json: {e}"))?;
    let sha = v
        .get("sha")
        .and_then(Value::as_str)
        .ok_or_else(|| "api json: missing sha".to_string())?;
    let b64 = v
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| "api json: missing content".to_string())?;
    let bytes = base64_decode(b64)?;
    let text = String::from_utf8(bytes.clone()).map_err(|e| format!("content utf8: {e}"))?;
    let got = git_blob_sha1(&bytes);
    if !got.eq_ignore_ascii_case(sha) {
        return Err(format!(
            "blob sha mismatch (integrity check failed): computed {got} != api {sha}"
        ));
    }
    Ok(text)
}

/// 预处理网络结果：github-api 源返回 JSON envelope，解码为原文（含 blob 校验）；
/// 校验失败 → 该源记为 Err（与网络失败同路径）。
fn normalize_results(
    results: Vec<(&'static str, Result<String, String>)>,
) -> Vec<(&'static str, Result<String, String>)> {
    results
        .into_iter()
        .map(|(name, r)| {
            let r = match r {
                Ok(text) if name == "github-api" => decode_github_contents(&text),
                other => other,
            };
            (name, r)
        })
        .collect()
}

/// 从并行结果中挑「任二一致」的正文（S2）。返回 (正文, 源名)；优先取排序靠前的源。
pub fn pick_agreed(
    results: &[(&'static str, Result<String, String>)],
) -> Option<(String, &'static str)> {
    for i in 0..results.len() {
        for j in (i + 1)..results.len() {
            if let (Ok(a), Ok(b)) = (&results[i].1, &results[j].1) {
                if a.trim_end() == b.trim_end() {
                    return Some((a.clone(), results[i].0));
                }
            }
        }
    }
    None
}

/// 单源兜底：**唯一**成功源恰为 github-api（正文已过 git blob sha1 校验）→ 采信。
/// 其余任何单源（raw/jsdelivr/aibox-im）一律不采信（S2 严格性不变）。
pub fn pick_single_api(
    results: &[(&'static str, Result<String, String>)],
) -> Option<String> {
    let mut oks = results.iter().filter(|(_, r)| r.is_ok());
    let first = oks.next()?;
    if oks.next().is_some() {
        return None; // 多于 1 个成功 → 交给任二交叉 / 不一致拒绝逻辑
    }
    if first.0 != "github-api" {
        return None;
    }
    first.1.as_ref().ok().cloned()
}

fn cache_path(name: &str) -> PathBuf {
    paths::registry_cache_dir().join(name)
}

/// 缓存文件 mtime 在 TTL 内。
pub fn cache_fresh(name: &str) -> bool {
    fresh_at(&cache_path(name), CACHE_TTL)
}

/// 通用 mtime 新鲜度（可注入时长，单测用）。
pub fn fresh_at(path: &Path, ttl: Duration) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    match modified.elapsed() {
        Ok(age) => age < ttl,
        Err(_) => true, // mtime 在未来（时钟回拨）→ 视为新鲜
    }
}

fn write_cache(name: &str, text: &str, source: &str) -> CoreResult<()> {
    let dir = paths::registry_cache_dir();
    paths::ensure_dir(&dir).map_err(|e| io_err("create registry cache", e))?;
    let p = cache_path(name);
    std::fs::write(&p, text).map_err(|e| io_err(format!("write {}", p.display()), e))?;
    let meta = serde_json::json!({
        "source": source,
        "fetchedAtMs": now_ms(),
    });
    let mp = p.with_extension("meta.json");
    let _ = std::fs::write(mp, meta.to_string());
    Ok(())
}

pub(crate) fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 读缓存正文（不要求新鲜；缺失 → `REGISTRY_NO_CACHE`）。
pub fn read_cache(name: &str) -> CoreResult<String> {
    let p = cache_path(name);
    std::fs::read_to_string(&p).map_err(|_| {
        CoreError::new(
            "REGISTRY_NO_CACHE",
            format!("Registry cache not available: {name}"),
        )
    })
}

/// 拉取 `rel`（如 `index.json`）：新鲜缓存 → 直接用；否则四源并行 + 任二一致，
/// 或 GitHub API 单源（blob 校验通过）兜底。
/// `force=true` 绕过缓存（「检查更新」按钮）；网络失败时回退旧缓存（篡改不一致除外）。
pub fn fetch_crosschecked(
    rel: &str,
    cache_name: &str,
    force: bool,
) -> CoreResult<(String, String)> {
    if !force && cache_fresh(cache_name) {
        return Ok((read_cache(cache_name)?, "cache".to_string()));
    }

    let sources = source_urls(rel);
    let results: Vec<(&'static str, Result<String, String>)> = std::thread::scope(|s| {
        let handles: Vec<_> = sources
            .iter()
            .map(|(name, url)| {
                let name = *name;
                let url = url.clone();
                s.spawn(move || (name, http_get(&url, name)))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| ("unknown", Err("thread panicked".into())))
            })
            .collect()
    });

    // 「响应成功」计数按 HTTP 层算：github-api 若校验失败仍算 responded
    // （→ 走 MISMATCH / SINGLE_SOURCE 既有错误路径，而不是伪装成 0 源）。
    let ok_count = results.iter().filter(|(_, r)| r.is_ok()).count();
    let results = normalize_results(results);

    if let Some((text, source)) = pick_agreed(&results) {
        let _ = write_cache(cache_name, &text, source)?;
        return Ok((text, source.to_string()));
    }

    // GitHub API 单源（其余源全挂）：正文已在 decode 阶段过 git blob sha1 完整性校验
    // → 采信（可用性兜底）。缓存 meta 标 github-api-single 以区分交叉一致来源。
    if let Some(text) = pick_single_api(&results) {
        let _ = write_cache(cache_name, &text, "github-api-single");
        return Ok((text, "github-api".to_string()));
    }

    let errs: Vec<String> = results
        .iter()
        .filter_map(|(n, r)| match r {
            Err(e) => Some(format!("{n}: {e}")),
            _ => None,
        })
        .collect();

    if ok_count >= 2 {
        // ≥2 源可达但内容互不一致 → 不采信（S2 防篡改），即使有旧缓存也不静默降级
        return Err(CoreError::new(
            "REGISTRY_MISMATCH",
            format!(
                "Registry sources disagree (S2 cross-check failed): {}",
                errs.join(" | ")
            ),
        ));
    }

    // 0~1 源可达 → 可用性回退旧缓存
    if let Ok(cached) = read_cache(cache_name) {
        return Ok((cached, "cache-stale".to_string()));
    }
    if ok_count == 0 {
        Err(CoreError::new(
            "REGISTRY_UNAVAILABLE",
            format!(
                "Registry unreachable (no source responded): {}",
                errs.join(" | ")
            ),
        ))
    } else {
        Err(CoreError::new(
            "REGISTRY_SINGLE_SOURCE",
            format!(
                "Only one registry source responded — cannot cross-check (S2): {}",
                errs.join(" | ")
            ),
        ))
    }
}

/// 解析正文为 JSON（registry 文档必须是合法 JSON）。
pub fn parse_doc(text: &str, what: &str) -> CoreResult<Value> {
    serde_json::from_str(text)
        .map_err(|e| CoreError::new("REGISTRY_BAD_JSON", format!("Invalid {what} document: {e}")))
}

/// `registry_index(force?)` → index.json 全文文档。
pub fn fetch_index(force: bool) -> CoreResult<(Value, String)> {
    let (text, src) = fetch_crosschecked("index.json", "index.json", force)?;
    Ok((parse_doc(&text, "index")?, src))
}

/// `registry_app(id)` → `apps/<id>/app.json` 全文文档。
pub fn fetch_app(id: &str, force: bool) -> CoreResult<(Value, String)> {
    validate_id(id)?;
    let rel = format!("apps/{id}/app.json");
    let (text, src) = fetch_crosschecked(&rel, &format!("app-{id}.json"), force)?;
    Ok((parse_doc(&text, "app manifest")?, src))
}

/// 读**缓存**的 app 文档（本地操作不联网；供 config_target 解析）。
pub fn read_cached_app(id: &str) -> CoreResult<Value> {
    validate_id(id)?;
    let text = read_cache(&format!("app-{id}.json"))?;
    parse_doc(&text, "app manifest")
}

/// `locales_load(lang)` → `locales/<lang>.json` 全文文档。
pub fn fetch_locales(lang: &str, force: bool) -> CoreResult<(Value, String)> {
    validate_lang(lang)?;
    let rel = format!("locales/{lang}.json");
    let (text, src) = fetch_crosschecked(&rel, &format!("locales-{lang}.json"), force)?;
    Ok((parse_doc(&text, "locales")?, src))
}

/// app id 白名单字符（防路径注入）。
pub fn validate_id(id: &str) -> CoreResult<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(CoreError::new(
            "REGISTRY_BAD_ID",
            format!("Invalid app id: {id}"),
        ))
    }
}

/// 语言标签白名单字符（防路径注入；registry 可扩任意语言）。
pub fn validate_lang(lang: &str) -> CoreResult<()> {
    let ok = !lang.is_empty()
        && lang.len() <= 16
        && lang
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(CoreError::new(
            "LOCALES_BAD_LANG",
            format!("Invalid language tag: {lang}"),
        ))
    }
}

/// 展平嵌套 JSON 为 `Record<string,string>`（locales 热加载；叶子键点路径）。
pub fn flatten_strings(v: &Value, prefix: &str, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten_strings(child, &path, out);
            }
        }
        Value::String(s) => {
            out.insert(prefix.to_string(), s.clone());
        }
        Value::Number(n) => {
            out.insert(prefix.to_string(), n.to_string());
        }
        Value::Bool(b) => {
            out.insert(prefix.to_string(), b.to_string());
        }
        // 数组：整段 JSON 字符串化（locales 不应出现数组，保底不丢键）
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

/// 展平入口。
pub fn flatten(v: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    flatten_strings(v, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_urls_cover_four_sources() {
        let s = source_urls("index.json");
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].0, "jsdelivr");
        assert!(s[0]
            .1
            .contains("cdn.jsdelivr.net/gh/imAiBOX/aibox-registry@main/index.json"));
        assert!(s[1]
            .1
            .contains("raw.githubusercontent.com/imAiBOX/aibox-registry/main/index.json"));
        assert!(s[2]
            .1
            .contains("api.github.com/repos/imAiBOX/aibox-registry/contents/index.json"));
        // 第 4 源：aibox.im 镜像（与 registry 同构路径 meta/<rel>）
        assert_eq!(s[3].0, "aibox-im");
        assert_eq!(s[3].1, "https://aibox.im/meta/index.json");
        // 路径映射：apps/<id>/app.json 与 locales/<lang>.json 同构直拼（两端一致约定）
        let app = source_urls("apps/openclaw/app.json");
        assert_eq!(app[3].1, "https://aibox.im/meta/apps/openclaw/app.json");
        let loc = source_urls("locales/zh-CN.json");
        assert_eq!(loc[3].1, "https://aibox.im/meta/locales/zh-CN.json");
    }

    #[test]
    fn git_blob_sha1_known_vectors() {
        // sha1("blob 5\0hello")
        assert_eq!(
            git_blob_sha1(b"hello"),
            "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0"
        );
        // sha1("blob 7\0{\"a\":1}")
        assert_eq!(
            git_blob_sha1(br#"{"a":1}"#),
            "daa5053ecf5f9a37b2de733d0751cc1ab53ac010"
        );
    }

    #[test]
    fn decode_github_contents_mock_envelope() {
        let content = r#"{"a":1}"#;
        let sha = git_blob_sha1(content.as_bytes());
        // 正常 envelope（content 为 base64）
        let ok_envelope = format!(r#"{{"content":"eyJhIjoxfQ==","sha":"{sha}","encoding":"base64"}}"#);
        assert_eq!(decode_github_contents(&ok_envelope).unwrap(), content);
        // GitHub 会在长 content 里折行（\r\n）——解码必须容忍
        let wrapped = format!(
            r#"{{"content":"eyJhIjoxf\r\nQ==","sha":"{sha}","encoding":"base64"}}"#
        );
        assert_eq!(decode_github_contents(&wrapped).unwrap(), content);
        // sha 不一致（篡改/损坏）→ 拒绝
        let tampered = format!(
            r#"{{"content":"eyJhIjoxfQ==","sha":"0000000000000000000000000000000000000000"}}"#
        );
        assert!(decode_github_contents(&tampered)
            .unwrap_err()
            .contains("mismatch"));
        // 缺 sha / 缺 content / 非 JSON → 拒绝
        assert!(decode_github_contents(r#"{"content":"eyJhIjoxfQ=="}"#).is_err());
        assert!(decode_github_contents(r#"{"sha":"x"}"#).is_err());
        assert!(decode_github_contents("not json").is_err());
        // 非法 base64 → 拒绝
        let bad_b64 = format!(r#"{{"content":"!!not-b64!!","sha":"{sha}"}}"#);
        assert!(decode_github_contents(&bad_b64).is_err());
    }

    #[test]
    fn pick_single_api_only_github_api_accepted() {
        // 唯一成功源 = github-api（已校验）→ 采信
        let r: Vec<(&str, Result<String, String>)> = vec![
            ("jsdelivr", Err("down".into())),
            ("raw", Err("down".into())),
            ("github-api", Ok("{\"a\":1}".to_string())),
            ("aibox-im", Err("down".into())),
        ];
        assert_eq!(pick_single_api(&r).unwrap(), "{\"a\":1}");
        // 唯一成功源 = raw（非 api）→ 仍拒绝（S2 严格性）
        let r2: Vec<(&str, Result<String, String>)> = vec![
            ("jsdelivr", Err("down".into())),
            ("raw", Ok("1".to_string())),
            ("github-api", Err("down".into())),
            ("aibox-im", Err("down".into())),
        ];
        assert!(pick_single_api(&r2).is_none());
        // 唯一成功源 = aibox-im（第 4 源，无 blob 校验）→ 不采信
        let r3: Vec<(&str, Result<String, String>)> = vec![
            ("jsdelivr", Err("down".into())),
            ("raw", Err("down".into())),
            ("github-api", Err("down".into())),
            ("aibox-im", Ok("1".to_string())),
        ];
        assert!(pick_single_api(&r3).is_none());
        // api 与另一源都通 → 不走单源兜底（交给任二交叉/不一致拒绝）
        let r4: Vec<(&str, Result<String, String>)> = vec![
            ("jsdelivr", Err("down".into())),
            ("raw", Ok("1".to_string())),
            ("github-api", Ok("2".to_string())),
            ("aibox-im", Err("down".into())),
        ];
        assert!(pick_single_api(&r4).is_none());
        // 全挂 → None
        let r5: Vec<(&str, Result<String, String>)> = vec![
            ("jsdelivr", Err("down".into())),
            ("raw", Err("down".into())),
            ("github-api", Err("down".into())),
            ("aibox-im", Err("down".into())),
        ];
        assert!(pick_single_api(&r5).is_none());
    }

    #[test]
    fn pick_agreed_any_two() {
        // jsdelivr 与 raw 一致 → 采信
        let r: Vec<(&str, Result<String, String>)> = vec![
            ("jsdelivr", Ok("{\"a\":1}".to_string())),
            ("raw", Ok("{\"a\":1}".to_string())),
            ("github-api", Ok("{\"a\":2}".to_string())),
        ];
        let got = pick_agreed(&r).unwrap();
        assert_eq!(got.1, "jsdelivr");
        assert_eq!(got.0, "{\"a\":1}");
        // 全不一致 → 不采信
        let r2: Vec<(&str, Result<String, String>)> = vec![
            ("jsdelivr", Ok("1".to_string())),
            ("raw", Ok("2".to_string())),
            ("github-api", Ok("3".to_string())),
        ];
        assert!(pick_agreed(&r2).is_none());
        // 只有一个成功 → 无法交叉
        let r3: Vec<(&str, Result<String, String>)> = vec![
            ("jsdelivr", Ok("1".to_string())),
            ("raw", Err("down".into())),
            ("github-api", Err("down".into())),
        ];
        assert!(pick_agreed(&r3).is_none());
    }

    #[test]
    fn id_and_lang_validation_blocks_traversal() {
        assert!(validate_id("openclaw").is_ok());
        assert!(validate_id("../etc/passwd").is_err());
        assert!(validate_id("").is_err());
        assert!(validate_lang("zh-CN").is_ok());
        assert!(validate_lang("../../x").is_err());
    }

    #[test]
    fn flatten_produces_dotted_keys() {
        let v = serde_json::json!({
            "meta": {"name": "AiBOX"},
            "screens": {"idle": {"title": "hi", "count": 3, "flag": true}},
            "list": [1, 2]
        });
        let f = flatten(&v);
        assert_eq!(f.get("meta.name").unwrap(), "AiBOX");
        assert_eq!(f.get("screens.idle.title").unwrap(), "hi");
        assert_eq!(f.get("screens.idle.count").unwrap(), "3");
        assert_eq!(f.get("screens.idle.flag").unwrap(), "true");
        assert!(f.contains_key("list"));
    }

    #[test]
    fn fresh_at_ttl_logic() {
        let d = std::env::temp_dir().join(format!("aibox-reg-fresh-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("x.json");
        std::fs::write(&f, "{}").unwrap();
        assert!(fresh_at(&f, Duration::from_secs(60)));
        assert!(!fresh_at(&f, Duration::from_secs(0)));
        assert!(!fresh_at(&d.join("missing"), Duration::from_secs(60)));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cache_roundtrip_offline() {
        let name = format!("test-{}.json", std::process::id());
        // 确保干净
        let _ = std::fs::remove_file(cache_path(&name));
        assert!(read_cache(&name).is_err());
        write_cache(&name, "{\"ok\":1}", "test").unwrap();
        assert_eq!(read_cache(&name).unwrap(), "{\"ok\":1}");
        assert!(cache_fresh(&name));
        let _ = std::fs::remove_file(cache_path(&name));
        let _ = std::fs::remove_file(cache_path(&name).with_extension("meta.json"));
    }
}
