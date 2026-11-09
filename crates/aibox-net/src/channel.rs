//! 通道类型：同一资源的多个下载来源，全通道共用同一 SHA256。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ChannelType {
    /// 官方源直连（nodejs.org、registry.npmjs.org、GitHub Releases 等）
    Direct,
    /// 机构运营镜像（阿里 npmmirror、清华 TUNA、中科大 USTC）
    Mirror,
    /// 社区 GitHub 加速网关前缀（ghproxy 类；仅用于 GitHub Release 资产）
    Proxy,
    /// 用户自定义加速前缀（设置中填写，启用时一次性安全提示）
    Custom,
}

impl ChannelType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ChannelType::Direct => "DIRECT",
            ChannelType::Mirror => "MIRROR",
            ChannelType::Proxy => "PROXY",
            ChannelType::Custom => "CUSTOM",
        }
    }

    pub fn all() -> [ChannelType; 4] {
        [
            ChannelType::Direct,
            ChannelType::Mirror,
            ChannelType::Proxy,
            ChannelType::Custom,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_type_variants_serialize_uppercase() {
        assert_eq!(
            ChannelType::all().map(|c| c.as_str()),
            ["DIRECT", "MIRROR", "PROXY", "CUSTOM"]
        );
        assert_eq!(
            serde_json::to_string(&ChannelType::Proxy).unwrap(),
            "\"PROXY\""
        );
        assert_eq!(
            serde_json::from_str::<ChannelType>("\"MIRROR\"").unwrap(),
            ChannelType::Mirror
        );
    }
}
