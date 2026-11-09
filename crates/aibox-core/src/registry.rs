//! registry 多源拉取与 12h 缓存（S2：jsdelivr / raw / GitHub API **任二一致**才采信）。
//!
//! - 缓存：`~/.aibox/cache/registry/<name>`，mtime 12h 内直接命中（`force=true` 绕过）
//! - 0~1 个源可达 → 退回缓存（可用性）；无缓存 → `REGISTRY_SINGLE_SOURCE`/`REGISTRY_UNAVAILABLE`
//! - ≥2 个源可达但互不一致 → `REGISTRY_MISMATCH`（**不采信**，防篡改）
//! - locales 也走同一套双源+缓存

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

/// 三源 URL（顺序即任务约定：jsdelivr → raw → GitHub API）。
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
    ]
}

/// 单次 GET（超时 8s，跟随重定向；GitHub API 带 UA + raw Accept）。
fn http_get(url: &str, source: &str) -> Result<String, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(SOURCE_TIMEOUT)
        .timeout_connect(SOURCE_TIMEOUT)
        .user_agent(USER_AGENT)
        .build();
    let mut req = agent.get(url).set("User-Agent", USER_AGENT);
    if source == "github-api" {
        req = req.set("Accept", "application/vnd.github.raw");
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

/// 拉取 `rel`（如 `index.json`）：新鲜缓存 → 直接用；否则三源并行 + 任二一致。
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

    let ok_count = results.iter().filter(|(_, r)| r.is_ok()).count();

    if let Some((text, source)) = pick_agreed(&results) {
        let _ = write_cache(cache_name, &text, source)?;
        return Ok((text, source.to_string()));
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
    fn source_urls_cover_three_sources() {
        let s = source_urls("index.json");
        assert_eq!(s.len(), 3);
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
