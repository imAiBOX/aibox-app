//! 下载原语：包装 aibox-net [`download_fallback`]（8MB 分段/断点/降级/S1 SHA-256）。
//!
//! dest 固定落 `~/.aibox/cache/downloads/<sha256><ext>`（内容寻址，旧 destName 自动迁移），
//! 进度经闭包上抛。

use std::path::{Path, PathBuf};

use aibox_net::{
    download_fallback, ChannelUrl, DownloadOptions, DownloadProgress, DownloadReceipt,
};

use crate::err::{io_err, CoreError, CoreResult};
use crate::paths;

/// 内容寻址缓存文件名：`<sha256><扩展名>`（扩展名取自 destName，保留安装器语义）。
/// 跨应用/重装/断点天然去重；TS 侧 `cacheFileName` 与其同构（改一处必改另一处）。
pub fn content_name(dest_name: &str, sha256: &str) -> String {
    let sha = sha256.trim().to_ascii_lowercase();
    let ext = match dest_name.rfind('.') {
        Some(i) if i > 0 && i + 1 < dest_name.len() => &dest_name[i..],
        _ => "",
    };
    format!("{sha}{ext}")
}

/// 解析通道列表（TS 传 `{type, url}[]`；type 大小写不敏感：registry 用小写）。
pub fn parse_channels(raw: &[serde_json::Value]) -> CoreResult<Vec<ChannelUrl>> {
    let mut out = Vec::with_capacity(raw.len());
    for item in raw {
        let channel = item
            .get("type")
            .and_then(|t| t.as_str())
            .ok_or_else(|| CoreError::new("DOWNLOAD_BAD_CHANNEL", "channel.type missing"))?;
        let ct = parse_channel_type(channel)?;
        let url = item
            .get("url")
            .and_then(|u| u.as_str())
            .ok_or_else(|| CoreError::new("DOWNLOAD_BAD_CHANNEL", "channel.url missing"))?;
        out.push(ChannelUrl::new(ct, url));
    }
    if out.is_empty() {
        return Err(CoreError::new(
            "DOWNLOAD_BAD_CHANNEL",
            "Download task has no channels",
        ));
    }
    Ok(out)
}

/// 宽容解析通道类型（`direct`/`DIRECT` 均可；registry 与 TS 两侧大小写不一致的缓冲）。
pub fn parse_channel_type(s: &str) -> CoreResult<aibox_net::ChannelType> {
    use aibox_net::ChannelType;
    match s.trim().to_ascii_lowercase().as_str() {
        "direct" => Ok(ChannelType::Direct),
        "mirror" => Ok(ChannelType::Mirror),
        "proxy" => Ok(ChannelType::Proxy),
        "custom" => Ok(ChannelType::Custom),
        other => Err(CoreError::new(
            "DOWNLOAD_BAD_CHANNEL",
            format!("Unknown channel type: {other}"),
        )),
    }
}

/// dest 落点校验：只允许文件名（禁止路径穿越）。
pub fn dest_for(dest_name: &str) -> CoreResult<PathBuf> {
    let name = dest_name.trim();
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('~')
    {
        return Err(CoreError::new(
            "DOWNLOAD_BAD_DEST",
            format!("Illegal destination file name: {dest_name}"),
        ));
    }
    let dir = paths::downloads_dir();
    paths::ensure_dir(&dir).map_err(|e| io_err("create download cache dir", e))?;
    Ok(dir.join(name))
}

/// 下载任务参数（DownloadTask 的 Rust 侧输入）。
pub struct DownloadSpec {
    pub channels: Vec<ChannelUrl>,
    pub sha256: String,
    /// 校验方式："bytes"（默认，校验下载字节的 sha256）| "content"（校验 gzip 内层内容摘要）
    ///
    /// **为什么需要**：GitHub 自动生成的源码包每次 fetch 重新 gzip → 字节摘要必然漂移；
    /// 内层 tar 内容稳定 → 用内容摘要校验，既不漂移又不丢完整性保证。
    pub hash_mode: Option<String>,
    pub dest_name: String,
    /// registry 包级期望字节数（`sizeBytes`，通道无关）；`None` = 不知道
    pub size_bytes: Option<u64>,
    /// 跳过的通道（「换个下载源」：用户显式换源重发时传入）
    pub skip_channels: Vec<aibox_net::ChannelType>,
    /// 所属应用 id（任务日志子目录 `~/.aibox/logs/<app>/`）
    pub app_id: Option<String>,
}

/// 按校验方式核对文件摘要。
///
/// - `hash_mode = "content"` → 校验 **gzip 内层内容** 的 sha256（源码归档用；外层 gzip 会漂移）
/// - `bytes` / None → 校验文件字节的 sha256；未知模式拒绝。
pub fn verify_digest(path: &Path, expected: &str, hash_mode: Option<&str>) -> bool {
    aibox_net::digest::DigestMode::parse(hash_mode)
        .ok()
        .and_then(|mode| mode.digest(path).ok())
        .map(|actual| actual.eq_ignore_ascii_case(expected.trim()))
        .unwrap_or(false)
}

/// 解析 `skipChannels`（TS 传通道类型名，大小写不敏感）。
pub fn parse_skip_channels(raw: &[String]) -> CoreResult<Vec<aibox_net::ChannelType>> {
    let mut out = Vec::with_capacity(raw.len());
    for s in raw {
        out.push(parse_channel_type(s)?);
    }
    Ok(out)
}

/// 应用跳过通道后的有效通道列表（空 → 无路可走，交由 `NET_NO_CHANNELS`）。
pub fn apply_skip(
    channels: Vec<ChannelUrl>,
    skip: &[aibox_net::ChannelType],
) -> Vec<ChannelUrl> {
    if skip.is_empty() {
        return channels;
    }
    let filtered: Vec<ChannelUrl> = channels
        .into_iter()
        .filter(|c| !skip.contains(&c.channel))
        .collect();
    filtered
}

/// 执行下载：dest 落 `~/.aibox/cache/downloads/<sha256><ext>`（内容寻址），
/// S1 全通道 SHA-256 零豁免。进度经 `progress` 闭包上抛（调用方映射为 `download_progress` 事件）。
///
/// **先查缓存**：目标内容寻址文件已存在且校验通过（或旧 destName 文件可迁移）
/// → 直接命中返回，零网络（预下载/重装/断点的「升级秒装」闭环）。
pub fn download_to_cache(
    spec: &DownloadSpec,
    progress: &mut dyn FnMut(DownloadProgress),
) -> CoreResult<DownloadReceipt> {
    download_to_cache_in(&paths::downloads_dir(), spec, progress, true)
}

fn download_to_cache_in(
    dir: &Path,
    spec: &DownloadSpec,
    progress: &mut dyn FnMut(DownloadProgress),
    detect_proxy: bool,
) -> CoreResult<DownloadReceipt> {
    if spec.sha256.trim().len() != 64 || !spec.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CoreError::new(
            "DOWNLOAD_BAD_SHA256",
            "Expected a 64-hex-char SHA-256 for this download",
        ));
    }
    let mode = aibox_net::digest::DigestMode::parse(spec.hash_mode.as_deref())
        .map_err(|e| CoreError::new("DOWNLOAD_BAD_HASH_MODE", e))?;
    let _ = dest_for(&spec.dest_name)?; // 名称合法性（防穿越）+ 目录确保存在
    let sha = spec.sha256.trim().to_ascii_lowercase();
    // 1) 缓存命中（含旧 destName → sha 迁移）→ 零网络直接过
    if let Some(hit) = resolve_cached(dir, &spec.dest_name, &sha, spec.hash_mode.as_deref())? {
        let bytes = std::fs::metadata(&hit).map(|m| m.len()).unwrap_or(0);
        let (channel, url) = spec
            .channels
            .first()
            .map(|c| (c.channel, c.url.clone()))
            .unwrap_or((aibox_net::ChannelType::Direct, String::new()));
        return Ok(DownloadReceipt {
            channel,
            url,
            bytes,
            sha256: sha,
            resumed_from: 0,
            get_requests: 0,
        });
    }
    let channels = apply_skip(spec.channels.clone(), &spec.skip_channels);
    if channels.is_empty() {
        return Err(CoreError::new(
            "DOWNLOAD_BAD_CHANNEL",
            "All download channels were skipped; no source left to try",
        ));
    }
    let mut opts = DownloadOptions::default();
    opts.digest_mode = mode;
    opts.expected_size = mode.expected_size(spec.size_bytes);
    // **系统代理**：用户在设置里开着「使用系统代理」→ 自动跟随（Windows 读 WinINET 注册表）。
    // 探测一次并缓存（代理很少变；每次下载都探会白等几秒）。
    if detect_proxy && crate::region::use_system_proxy() {
        opts.proxy = system_proxy_cached();
    }
    let dest = dir.join(content_name(&spec.dest_name, &sha));
    let receipt = download_fallback(&channels, &dest, &sha, &opts, progress)?;
    Ok(receipt)
}

/// 进程内缓存的系统代理（`None` = 探测过且没有可用代理）。
///
/// 只探测一次：代理连通性检查要 TCP + CONNECT 握手，每次下载都探会白等几秒。
fn system_proxy_cached() -> Option<aibox_net::ProxyConfig> {
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<Option<Option<aibox_net::ProxyConfig>>>> = OnceLock::new();
    let cell = CACHE.get_or_init(|| Mutex::new(None));
    let mut g = cell.lock().unwrap_or_else(|p| p.into_inner());
    if g.is_none() {
        let mut candidates = crate::proxy_sys::detect_system_proxies();
        candidates.extend(aibox_net::proxy::detect_env_proxies());
        let liveness: Vec<aibox_net::ProxyLiveness> = candidates
            .iter()
            .map(|p| {
                aibox_net::proxy::check_proxy(p, "github.com:443", std::time::Duration::from_secs(3))
            })
            .collect();
        *g = Some(aibox_net::proxy::decide_proxy(&candidates, &liveness).cloned());
    }
    g.clone().flatten()
}

/// 内容寻址缓存命中检查（可注入目录，单测离线）：
/// 1. `<sha><ext>` 存在且校验通过 → 命中；校验不过 → **删除**（不留毒）；
/// 2. 旧 `destName` 文件存在且校验通过 → 重命名迁移为内容寻址文件名（兼容老版本缓存）；
/// 3. 都不可用 → `None`（走网络下载）。
pub fn resolve_cached(
    dir: &Path,
    dest_name: &str,
    sha256: &str,
    hash_mode: Option<&str>,
) -> CoreResult<Option<PathBuf>> {
    let sha = sha256.trim().to_ascii_lowercase();
    if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CoreError::new(
            "DOWNLOAD_BAD_SHA256",
            "Expected a 64-hex-char SHA-256 for this download",
        ));
    }
    if dest_name.is_empty()
        || dest_name.contains('/')
        || dest_name.contains('\\')
        || dest_name.contains("..")
    {
        return Err(CoreError::new(
            "DOWNLOAD_BAD_DEST",
            format!("Illegal destination file name: {dest_name}"),
        ));
    }
    let content = dir.join(content_name(dest_name, &sha));
    if content.is_file() {
        if verify_digest(&content, &sha, hash_mode) {
            return Ok(Some(content));
        }
        let _ = std::fs::remove_file(&content); // 校验失败删文件不留毒
    }
    // 旧 destName 文件（老版本缓存）：校验通过 → 迁移到内容寻址文件名
    let legacy = dir.join(dest_name);
    if legacy != content && legacy.is_file() && verify_digest(&legacy, &sha, hash_mode) {
        let _ = std::fs::remove_file(&content); // 清掉可能的坏残留
        if std::fs::rename(&legacy, &content).is_ok() {
            return Ok(Some(content));
        }
    }
    Ok(None)
}

/// 下载失败后的清毒：内容寻址文件存在但校验不过 → 删除（best-effort，幂等）。
pub fn purge_invalid(dir: &Path, dest_name: &str, sha256: &str) {
    purge_invalid_with_mode(dir, dest_name, sha256, None);
}

pub fn purge_invalid_with_mode(dir: &Path, dest_name: &str, sha256: &str, hash_mode: Option<&str>) {
    let sha = sha256.trim().to_ascii_lowercase();
    if sha.len() != 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return;
    }
    let content = dir.join(content_name(dest_name, &sha));
    if content.is_file() && !verify_digest(&content, &sha, hash_mode) {
        let _ = std::fs::remove_file(&content);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gzip(payload: &[u8], name: &str) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::GzBuilder::new()
            .filename(name)
            .write(Vec::new(), flate2::Compression::fast());
        encoder.write_all(payload).unwrap();
        encoder.finish().unwrap()
    }

    fn fixture_url(body: Vec<u8>) -> String {
        fixture_transport(body, 0)
    }

    fn fixture_transport(body: Vec<u8>, get_length_delta: usize) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/fixture.gz", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming().take(4) {
                let mut stream = stream.unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut req = Vec::new();
                let mut byte = [0];
                while !req.ends_with(b"\r\n\r\n") {
                    if stream.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    req.push(byte[0]);
                }
                let declared = body.len()
                    + if req.starts_with(b"HEAD") {
                        0
                    } else {
                        get_length_delta
                    };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    declared
                )
                .unwrap();
                if !req.starts_with(b"HEAD") {
                    let _ = stream.write_all(&body);
                }
            }
        });
        url
    }

    #[test]
    fn content_digest_download_cache_and_rejection_end_to_end() {
        let dir = tmp_dir("content-e2e");
        let payload = b"same archive tar bytes".repeat(100);
        let sha = aibox_net::sha256_hex(&payload);
        let a = gzip(&payload, "a");
        let b = gzip(&payload, "a much longer gzip filename");
        assert_ne!(a.len(), b.len());
        let mut spec = DownloadSpec {
            channels: vec![ChannelUrl::new(
                aibox_net::ChannelType::Direct,
                fixture_url(b.clone()),
            )],
            sha256: sha.clone(),
            hash_mode: Some("content".into()),
            dest_name: "fixture.tar.gz".into(),
            size_bytes: Some(a.len() as u64),
            skip_channels: vec![],
            app_id: None,
        };
        let receipt = download_to_cache_in(&dir, &spec, &mut |_| {}, false).unwrap();
        assert_eq!(receipt.sha256, sha);
        assert_eq!(receipt.bytes, b.len() as u64);
        let dest = dir.join(content_name(&spec.dest_name, &sha));
        assert_eq!(std::fs::read(&dest).unwrap(), b);
        purge_invalid_with_mode(&dir, &spec.dest_name, &sha, Some("content"));
        assert!(dest.exists());
        spec.channels[0].url = "http://127.0.0.1:1/unreachable".into();
        assert_eq!(
            download_to_cache_in(&dir, &spec, &mut |_| {}, false)
                .unwrap()
                .get_requests,
            0
        );

        // Even a valid inner digest cannot bypass inconsistent transport length.
        std::fs::remove_file(&dest).unwrap();
        spec.channels[0].url = fixture_transport(b.clone(), 5);
        assert!(download_to_cache_in(&dir, &spec, &mut |_| {}, false).is_err());
        assert!(!dest.exists());

        // Bytes mode still verifies compressed bytes, never the inner digest.
        spec.hash_mode = None;
        spec.size_bytes = Some(b.len() as u64);
        spec.channels[0].url = fixture_url(b.clone());
        assert_eq!(
            download_to_cache_in(&dir, &spec, &mut |_| {}, false)
                .unwrap_err()
                .code,
            "NET_HASH_MISMATCH"
        );
        assert!(!dest.exists());

        spec.hash_mode = Some("content".into());
        // Hashing just the first member would accept this malicious append.
        let mut appended = b.clone();
        appended.extend(gzip(b"extra untrusted content", "extra"));
        spec.channels[0].url = fixture_url(appended.clone());
        assert_eq!(
            download_to_cache_in(&dir, &spec, &mut |_| {}, false)
                .unwrap_err()
                .code,
            "NET_HASH_MISMATCH"
        );
        assert!(!dest.exists());
        let mut all = payload.clone();
        all.extend_from_slice(b"extra untrusted content");
        spec.sha256 = aibox_net::sha256_hex(&all);
        spec.channels[0].url = fixture_url(appended);
        assert!(download_to_cache_in(&dir, &spec, &mut |_| {}, false).is_ok());

        for (i, body) in [
            [b.clone(), b"junk".to_vec()].concat(),
            b[..b.len() - 3].to_vec(),
            {
                let mut bad = b.clone();
                let n = bad.len();
                bad[n - 8] ^= 1;
                bad
            },
            b"<html>gateway error</html>".to_vec(),
        ]
        .into_iter()
        .enumerate()
        {
            spec.sha256 = format!("{:064x}", i + 1);
            spec.channels[0].url = fixture_url(body);
            assert!(download_to_cache_in(&dir, &spec, &mut |_| {}, false).is_err());
            assert!(!dir
                .join(content_name(&spec.dest_name, &spec.sha256))
                .exists());
        }
        spec.hash_mode = Some("unknown".into());
        assert_eq!(
            download_to_cache_in(&dir, &spec, &mut |_| {}, false)
                .unwrap_err()
                .code,
            "DOWNLOAD_BAD_HASH_MODE"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_channels_case_insensitive() {
        let raw = serde_json::json!([
            {"type": "direct", "url": "https://a.example/x"},
            {"type": "MIRROR", "url": "https://b.example/x"}
        ]);
        let ch = parse_channels(raw.as_array().unwrap()).unwrap();
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[0].channel, aibox_net::ChannelType::Direct);
        assert_eq!(ch[1].channel, aibox_net::ChannelType::Mirror);
    }

    #[test]
    fn parse_channels_rejects_empty_and_bad_type() {
        assert!(parse_channels(&[]).is_err());
        let raw = serde_json::json!([{"type": "warp", "url": "https://x"}]);
        assert!(parse_channels(raw.as_array().unwrap()).is_err());
        let raw = serde_json::json!([{"type": "direct"}]);
        assert!(parse_channels(raw.as_array().unwrap()).is_err());
    }

    #[test]
    fn dest_name_rejects_traversal() {
        assert!(dest_for("node.msi").is_ok());
        assert!(dest_for("../evil").is_err());
        assert!(dest_for("a/b.msi").is_err());
        assert!(dest_for("a\\b.msi").is_err());
        assert!(dest_for("").is_err());
    }

    #[test]
    fn parse_skip_channels_case_insensitive() {
        let skip = parse_skip_channels(&["PROXY".into(), "proxy".into()]).unwrap();
        assert_eq!(
            skip,
            vec![
                aibox_net::ChannelType::Proxy,
                aibox_net::ChannelType::Proxy
            ]
        );
        assert!(parse_skip_channels(&["warp".into()]).is_err());
        assert!(parse_skip_channels(&[]).unwrap().is_empty());
    }

    #[test]
    fn apply_skip_drops_named_channels() {
        let raw = serde_json::json!([
            {"type": "proxy", "url": "https://p.example/x"},
            {"type": "direct", "url": "https://d.example/x"}
        ]);
        let ch = parse_channels(raw.as_array().unwrap()).unwrap();
        // 跳过 PROXY → 只剩 DIRECT（「换个下载源」语义）
        let out = apply_skip(ch.clone(), &[aibox_net::ChannelType::Proxy]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].channel, aibox_net::ChannelType::Direct);
        // 无跳过 → 原样
        assert_eq!(apply_skip(ch.clone(), &[]).len(), 2);
        // 全部跳过 → 空（上层报 DOWNLOAD_BAD_CHANNEL）
        assert!(apply_skip(
            ch,
            &[aibox_net::ChannelType::Proxy, aibox_net::ChannelType::Direct]
        )
        .is_empty());
    }

    #[test]
    fn content_name_is_sha_plus_ext() {
        let sha = "ab".repeat(32);
        assert_eq!(content_name("Hub-Setup.exe", &sha), format!("{sha}.exe"));
        assert_eq!(content_name("node-v26.msi", &sha), format!("{sha}.msi"));
        assert_eq!(content_name("noext", &sha), sha);
        assert_eq!(content_name(".hidden", &sha), sha, "前导点不算扩展名");
        assert_eq!(content_name("tail.", &sha), sha, "空扩展名不拼");
        // 大小写不敏感（上游 sha 可能大写）
        assert_eq!(
            content_name("a.bin", &"AB".repeat(32)),
            format!("{}.bin", "ab".repeat(32))
        );
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aibox-cad-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    const HELLO_SHA: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    #[test]
    fn resolve_cached_hits_content_addressed_file() {
        let d = tmp_dir("hit");
        let content = d.join(content_name("app.exe", HELLO_SHA));
        std::fs::write(&content, b"hello").unwrap();
        let got = resolve_cached(&d, "app.exe", HELLO_SHA, None).unwrap();
        assert_eq!(got, Some(content.clone()), "内容寻址文件存在且校验过 → 命中");
        // 重复检查：仍命中，不多出文件（幂等）
        let got2 = resolve_cached(&d, "app.exe", HELLO_SHA, None).unwrap();
        assert_eq!(got2, Some(content.clone()));
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "重复下载不重复存");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_cached_migrates_legacy_dest_name() {
        let d = tmp_dir("migrate");
        let legacy = d.join("app.exe");
        std::fs::write(&legacy, b"hello").unwrap();
        let got = resolve_cached(&d, "app.exe", HELLO_SHA, None).unwrap();
        let content = d.join(content_name("app.exe", HELLO_SHA));
        assert_eq!(got, Some(content.clone()), "旧 destName 校验过 → 迁移并命中");
        assert!(!legacy.exists(), "迁移后旧文件名不残留");
        assert!(content.is_file());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_cached_rejects_mismatch_and_purges_poison() {
        let d = tmp_dir("poison");
        // 内容寻址文件存在但内容不对 → 删除（不留毒）且不命中
        let content = d.join(content_name("app.exe", HELLO_SHA));
        std::fs::write(&content, b"evil").unwrap();
        assert!(resolve_cached(&d, "app.exe", HELLO_SHA, None).unwrap().is_none());
        assert!(!content.exists(), "校验失败删文件不留毒");
        // 旧文件内容不对 → 不迁移、不命中（留给网络重下）
        let legacy = d.join("app.exe");
        std::fs::write(&legacy, b"evil").unwrap();
        assert!(resolve_cached(&d, "app.exe", HELLO_SHA, None).unwrap().is_none());
        assert!(legacy.exists(), "不删非内容寻址的未知文件");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn purge_invalid_removes_bad_keeps_good() {
        let d = tmp_dir("purge");
        let bad = d.join(content_name("a.bin", HELLO_SHA));
        std::fs::write(&bad, b"nope").unwrap();
        purge_invalid(&d, "a.bin", HELLO_SHA);
        assert!(!bad.exists(), "坏文件被清理");
        std::fs::write(&bad, b"hello").unwrap();
        purge_invalid(&d, "a.bin", HELLO_SHA);
        assert!(bad.exists(), "校验通过的文件保留");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn resolve_cached_rejects_bad_inputs() {
        let d = tmp_dir("bad");
        assert!(resolve_cached(&d, "../x", HELLO_SHA, None).is_err());
        assert!(resolve_cached(&d, "a/b", HELLO_SHA, None).is_err());
        assert!(resolve_cached(&d, "a.bin", "zz", None).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
