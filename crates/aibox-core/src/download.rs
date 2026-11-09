//! 下载原语：包装 aibox-net [`download_fallback`]（8MB 分段/断点/降级/S1 SHA-256）。
//!
//! dest 固定落 `~/.aibox/cache/downloads/<destName>`，进度经闭包上抛。

use std::path::PathBuf;

use aibox_net::{
    download_fallback, ChannelUrl, DownloadOptions, DownloadProgress, DownloadReceipt,
};

use crate::err::{io_err, CoreError, CoreResult};
use crate::paths;

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
    pub dest_name: String,
}

/// 执行下载：dest 落 `~/.aibox/cache/downloads/<destName>`，S1 全通道 SHA-256 零豁免。
/// 进度经 `progress` 闭包上抛（调用方映射为 `download_progress` 事件）。
pub fn download_to_cache(
    spec: &DownloadSpec,
    progress: &mut dyn FnMut(DownloadProgress),
) -> CoreResult<DownloadReceipt> {
    if spec.sha256.trim().len() != 64 || !spec.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(CoreError::new(
            "DOWNLOAD_BAD_SHA256",
            "Expected a 64-hex-char SHA-256 for this download",
        ));
    }
    let dest = dest_for(&spec.dest_name)?;
    let opts = DownloadOptions::default();
    let receipt = download_fallback(
        &spec.channels,
        &dest,
        &spec.sha256.trim().to_ascii_lowercase(),
        &opts,
        progress,
    )?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
