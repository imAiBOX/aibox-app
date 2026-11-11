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
    /// registry 包级期望字节数（`sizeBytes`，通道无关）；`None` = 不知道
    pub size_bytes: Option<u64>,
    /// 跳过的通道（「换个下载源」：用户显式换源重发时传入）
    pub skip_channels: Vec<aibox_net::ChannelType>,
    /// 所属应用 id（任务日志子目录 `~/.aibox/logs/<app>/`）
    pub app_id: Option<String>,
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
    let channels = apply_skip(spec.channels.clone(), &spec.skip_channels);
    if channels.is_empty() {
        return Err(CoreError::new(
            "DOWNLOAD_BAD_CHANNEL",
            "All download channels were skipped; no source left to try",
        ));
    }
    let mut opts = DownloadOptions::default();
    opts.expected_size = spec.size_bytes;
    let receipt = download_fallback(
        &channels,
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
}
