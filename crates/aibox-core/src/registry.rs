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
/// UA 版本号随发布 tag 注入（scripts/set-version.mjs），不要再写死。
const USER_AGENT: &str = concat!("aibox-app/", env!("CARGO_PKG_VERSION"));

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

/// 完整性校验错误的结构化前缀。GitHub API 路径上的**任何**响应验证失败
/// （信封畸形、base64 非法、SHA 不符、内容非 UTF-8）都带此标签 → 判定为安全错误，
/// 整体拒绝，绝不回退旧缓存/嵌入快照。用前缀而非子串匹配，避免文案变更导致漏判。
pub const INTEGRITY_ERR_TAG: &str = "[integrity]";

fn integrity_err(msg: impl std::fmt::Display) -> String {
    format!("{INTEGRITY_ERR_TAG} {msg}")
}

/// GitHub contents API JSON envelope → 原文，并做完整性校验。
/// **顺序很重要**：先对原始字节算 git blob sha1 并与响应 `sha` 比对，
/// **通过之后**才做 UTF-8 转换——否则非 UTF-8 载荷会以 "utf8" 错误逃过完整性分类。
pub fn decode_github_contents(raw: &str) -> Result<String, String> {
    let v: Value = serde_json::from_str(raw).map_err(|e| integrity_err(format!("api json: {e}")))?;
    let sha = v
        .get("sha")
        .and_then(Value::as_str)
        .ok_or_else(|| integrity_err("api json: missing sha"))?;
    let b64 = v
        .get("content")
        .and_then(Value::as_str)
        .ok_or_else(|| integrity_err("api json: missing content"))?;
    let bytes = base64_decode(b64).map_err(|e| integrity_err(e))?;
    // ① 先验哈希（原始字节，未做任何编码转换）
    let got = git_blob_sha1(&bytes);
    if !got.eq_ignore_ascii_case(sha) {
        return Err(integrity_err(format!(
            "blob sha mismatch: computed {got} != api {sha}"
        )));
    }
    // ② 哈希通过后才是 UTF-8（失败同样是完整性问题：内容不是预期文档）
    String::from_utf8(bytes).map_err(|e| integrity_err(format!("content utf8: {e}")))
}

/// GitHub API 完整性校验失败判定（纯函数，单测覆盖）。
/// 这类失败是**安全错误**：必须整体拒绝，绝不回退旧缓存/嵌入快照。
pub fn is_api_integrity_failure(name: &str, r: &Result<String, String>) -> bool {
    name == "github-api" && matches!(r, Err(e) if e.starts_with(INTEGRITY_ERR_TAG))
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

    // Keep HTTP reachability separate from integrity validation: a GitHub blob
    // checksum failure is a hard security error and must never be hidden by a
    // stale cache fallback.
    let http_ok_count = results.iter().filter(|(_, r)| r.is_ok()).count();
    let api_integrity_error = results.iter().any(|(name, r)| is_api_integrity_failure(name, r));
    let results = normalize_results(results);
    let api_integrity_error =
        api_integrity_error || results.iter().any(|(name, r)| is_api_integrity_failure(name, r));
    if api_integrity_error {
        return Err(CoreError::new(
            "REGISTRY_INTEGRITY",
            "GitHub API content failed its git-blob SHA-1 integrity check; refusing all cache fallbacks",
        ));
    }

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

    if http_ok_count >= 2 {
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
    if http_ok_count == 0 {
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

/// 该来源是不是**网络核过的**（可用于安全敏感判定，如还原白名单）。
///
/// **允许列表**（不是黑名单！）：只有这 4 个**真网络来源**算数——
/// `jsdelivr` / `raw` / `github-api` / `aibox-im`。
///
/// **为什么不用黑名单**（审计 P2-1）：黑名单（"排除 cache / cache-stale"）在**新增来源**时
/// 会**默认放行**（fail-open）——例如构建期内嵌的本地快照 `snapshot`（网络全挂时的兜底）
/// 就会被当成「网络核过」→ 白名单可被本地文件投毒。
/// 允许列表的默认是「拒绝」→ 新增来源必须**显式**加进来。
///
/// 本地来源一律 `false`：`cache`（新鲜缓存）/ `cache-stale`（断网时的旧缓存）/ `snapshot`
/// （随版本发布的内嵌快照）/ 任何未知值。
///
/// 白名单必须走这条判定：缓存 / 快照里的 registry 文档可能被改过 → 还原目标可被换成 `~/.ssh`。
pub fn is_network_verified_source(src: &str) -> bool {
    matches!(src, "jsdelivr" | "raw" | "github-api" | "aibox-im")
}

/// 构建时嵌入的目录快照（`scripts/gen-registry-snapshot.mjs` 拉 registry index.json
/// 写入 `src/assets/registry-snapshot.json`，随版本发布；网络失败沿用仓库内上一次快照）。
/// 本地缓存与全部网络源都不可用时的最后兑底 → **断网首开也有目录**。
pub const INDEX_SNAPSHOT: &str = include_str!("../../../src/assets/registry-snapshot.json");

/// 可用性类失败（0~1 源可达且无缓存）→ 允许回落嵌入快照；
/// 源间互不一致（`REGISTRY_MISMATCH`，防篡改）等拒绝类错误**不**回落。
pub fn is_availability_err(e: &CoreError) -> bool {
    matches!(
        e.code.as_str(),
        "REGISTRY_UNAVAILABLE" | "REGISTRY_SINGLE_SOURCE"
    )
}

/// 解析正文为 JSON（registry 文档必须是合法 JSON）。
pub fn parse_doc(text: &str, what: &str) -> CoreResult<Value> {
    serde_json::from_str(text)
        .map_err(|e| CoreError::new("REGISTRY_BAD_JSON", format!("Invalid {what} document: {e}")))
}

/// `registry_index(force?)` → index.json 全文文档。
/// 降级链：新鲜缓存 → 多源交叉 → 旧缓存（cache-stale）→ **嵌入快照**（`source = "snapshot"`）。
pub fn fetch_index(force: bool) -> CoreResult<(Value, String)> {
    match fetch_crosschecked("index.json", "index.json", force) {
        Ok((text, src)) => Ok((parse_doc(&text, "index")?, src)),
        Err(e) if is_availability_err(&e) => {
            // 网络全挂且无本地缓存（新机器首装/断网）→ 构建时嵌入的快照兑底
            Ok((
                parse_doc(INDEX_SNAPSHOT, "registry snapshot")?,
                "snapshot".to_string(),
            ))
        }
        Err(e) => Err(e),
    }
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

    #[test]
    fn embedded_snapshot_parses_with_apps() {
        // 构建时嵌入的快照必须是合法 index 文档（scripts/gen-registry-snapshot.mjs 产物）
        let doc = parse_doc(INDEX_SNAPSHOT, "registry snapshot").expect("snapshot must parse");
        assert!(
            doc.get("apps").and_then(|a| a.as_array()).is_some(),
            "快照必须含 apps 数组"
        );
    }

    #[test]
    fn availability_err_gates_snapshot_fallback() {
        // 可用性失败（源全挂/单源无缓存）→ 允许嵌入快照
        assert!(is_availability_err(&CoreError::new(
            "REGISTRY_UNAVAILABLE",
            "x"
        )));
        assert!(is_availability_err(&CoreError::new(
            "REGISTRY_SINGLE_SOURCE",
            "x"
        )));
        // 源间不一致（防篡改）/其他错误 → 拒绝回落
        assert!(!is_availability_err(&CoreError::new("REGISTRY_MISMATCH", "x")));
        assert!(!is_availability_err(&CoreError::new("REGISTRY_BAD_JSON", "x")));
    }

    /// 审计回归 P2：GitHub API 完整性失败必须判为安全错误（禁止缓存降级）。
    #[test]
    fn api_integrity_failure_detection() {
        let tagged: Result<String, String> =
            Err(format!("{INTEGRITY_ERR_TAG} blob sha mismatch: computed a != api b"));
        assert!(is_api_integrity_failure("github-api", &tagged));
        // 非 API 源不带该语义
        assert!(!is_api_integrity_failure("raw", &tagged));
        // 网络失败必须走可用性降级，不得误判为完整性错误
        let net_err: Result<String, String> = Err("timed out".into());
        assert!(!is_api_integrity_failure("github-api", &net_err));
        assert!(!is_api_integrity_failure("github-api", &Ok("{}".into())));
    }

    /// 审计回归 P2（第二轮）：SHA 必须先于 UTF-8 校验——
    /// 非 UTF-8 载荷 + 错误 SHA 仍须判为完整性失败（旧实现会漏判并回落缓存）。
    #[test]
    fn non_utf8_payload_with_bad_sha_is_still_integrity_failure() {
        // 0xFF 0xFE 不是合法 UTF-8；base64: "//4="
        let raw = r#"{"sha":"0000000000000000000000000000000000000000","content":"//4="}"#;
        let r = decode_github_contents(raw);
        assert!(r.is_err(), "must reject");
        let e = r.unwrap_err();
        assert!(e.starts_with(INTEGRITY_ERR_TAG), "must be tagged integrity: {e}");
        assert!(is_api_integrity_failure("github-api", &Err(e)));
    }

    /// 缓存不是网络核过的来源（安全敏感判定必须拒绝）。
    #[test]
    fn cache_sources_are_not_network_verified() {
        assert!(!is_network_verified_source("cache"));
        assert!(!is_network_verified_source("cache-stale"));
        assert!(is_network_verified_source("github-api"));
        assert!(is_network_verified_source("raw"));
        assert!(is_network_verified_source("jsdelivr"));
        assert!(is_network_verified_source("aibox-im"));
    }

    /// **允许列表**语义（审计 P2-1）：本地来源 / 未知来源一律不算「网络核过」。
    ///
    /// 黑名单会在**新增来源**时 fail-open（默认放行）→ 内嵌快照 `snapshot`
    /// （构建时打进安装包的 registry 目录）就会被当成网络源 → 白名单可被投毒。
    #[test]
    fn only_the_four_network_sources_count_as_verified() {
        // 本地来源
        assert!(!is_network_verified_source("snapshot"));
        assert!(!is_network_verified_source("cache"));
        assert!(!is_network_verified_source("cache-stale"));
        // 未知 / 空 / 大小写变体 → 一律拒绝（允许列表的默认是「拒绝」）
        assert!(!is_network_verified_source(""));
        assert!(!is_network_verified_source("github-api "));
        assert!(!is_network_verified_source("GitHub-API"));
        assert!(!is_network_verified_source("https://evil.example/app.json"));
        assert!(!is_network_verified_source("file:///etc/passwd"));
        // 四个真网络源
        for src in ["jsdelivr", "raw", "github-api", "aibox-im"] {
            assert!(is_network_verified_source(src), "{src} must be verified");
        }
    }

    /// 正确 SHA + 非 UTF-8 内容 → 仍是完整性问题（内容不符预期文档）。
    #[test]
    fn correct_sha_non_utf8_content_is_integrity_failure() {
        let bytes = [0xFFu8, 0xFE];
        let sha = git_blob_sha1(&bytes);
        let b64 = "//4=";
        let raw = format!(r#"{{"sha":"{sha}","content":"{b64}"}}"#);
        let e = decode_github_contents(&raw).unwrap_err();
        assert!(e.starts_with(INTEGRITY_ERR_TAG), "utf8 failure must carry tag: {e}");
    }
}
