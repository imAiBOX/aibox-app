//! 资源-通道矩阵（数据驱动：registry 数据优先；**内置 v1.1 基线兜底**）。
//!
//! 依赖方向约束：core 不依赖 netdoctor，这里产出 [`ProbeTarget`]，
//! 由 src-tauri 转换为 `aibox_netdoctor::ChannelTarget`（net_probe 与 D4 同源）。

use aibox_net::ChannelType;
use serde::Serialize;

use crate::download::parse_channel_type;
use crate::registry;

/// 一条待测通道（与 netdoctor `ChannelTarget` 字段同构，壳侧一行转换）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeTarget {
    pub resource: String,
    pub channel: ChannelType,
    pub name: String,
    pub url: String,
}

/// 内置 v1.1 基线（registry 不可达时的诊断兜底）。
pub fn baseline_targets() -> Vec<ProbeTarget> {
    let mk = |resource: &str, channel: ChannelType, name: &str, url: &str| ProbeTarget {
        resource: resource.into(),
        channel,
        name: name.into(),
        url: url.into(),
    };
    vec![
        mk(
            "registry JSON",
            ChannelType::Direct,
            "raw.github",
            "https://raw.githubusercontent.com/imAiBOX/aibox-registry/main/index.json",
        ),
        mk(
            "registry JSON",
            ChannelType::Mirror,
            "jsDelivr",
            "https://cdn.jsdelivr.net/gh/imAiBOX/aibox-registry@main/index.json",
        ),
        mk(
            "registry JSON",
            ChannelType::Direct,
            "GitHub API",
            "https://api.github.com/repos/imAiBOX/aibox-registry/contents/index.json",
        ),
        mk(
            "Node.js dist",
            ChannelType::Direct,
            "nodejs.org",
            "https://nodejs.org/dist/index.json",
        ),
        mk(
            "Node.js dist",
            ChannelType::Mirror,
            "npmmirror.com",
            "https://npmmirror.com/mirrors/node/index.json",
        ),
        mk(
            "openclaw CLI (npm)",
            ChannelType::Direct,
            "registry.npmjs.org",
            "https://registry.npmjs.org/openclaw",
        ),
        mk(
            "openclaw CLI (npm)",
            ChannelType::Mirror,
            "registry.npmmirror.com",
            "https://registry.npmmirror.com/openclaw",
        ),
        mk(
            "Git for Windows",
            ChannelType::Direct,
            "github.com",
            "https://github.com/git-for-windows/git/releases/latest",
        ),
        mk(
            "Git for Windows",
            ChannelType::Proxy,
            "ghfast.top",
            "https://ghfast.top/https://github.com/git-for-windows/git/releases/latest",
        ),
        mk(
            "Git for Windows",
            ChannelType::Proxy,
            "gh-proxy.com",
            "https://gh-proxy.com/https://github.com/git-for-windows/git/releases/latest",
        ),
        mk(
            "OpenClaw Windows Hub",
            ChannelType::Direct,
            "github.com",
            "https://github.com/openclaw/openclaw-windows-node/releases/latest",
        ),
        mk(
            "OpenClaw Windows Hub",
            ChannelType::Proxy,
            "ghfast.top",
            "https://ghfast.top/https://github.com/openclaw/openclaw-windows-node/releases/latest",
        ),
        mk(
            "OpenClaw Windows Hub",
            ChannelType::Proxy,
            "gh-proxy.com",
            "https://gh-proxy.com/https://github.com/openclaw/openclaw-windows-node/releases/latest",
        ),
    ]
}

/// registry 文档 → 矩阵行（index 三源 + 各 app packages[].channels[]）。
pub fn registry_targets(index: &serde_json::Value, apps: &[serde_json::Value]) -> Vec<ProbeTarget> {
    let _ = index; // index 只用于确认 registry 可达；通道行来自源 URL 与 packages
    let mut out = Vec::new();
    for (name, url) in registry::source_urls("index.json") {
        let channel = match name {
            "jsdelivr" => ChannelType::Mirror,
            _ => ChannelType::Direct,
        };
        out.push(ProbeTarget {
            resource: "registry JSON".into(),
            channel,
            name: name.to_string(),
            url,
        });
    }
    for app in apps {
        let app_name = app
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("app")
            .to_string();
        let Some(packages) = app.get("packages").and_then(|p| p.as_array()) else {
            continue;
        };
        for pkg in packages {
            let pkg_name = pkg
                .get("name")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| {
                    pkg.get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("package")
                        .to_string()
                });
            let Some(channels) = pkg.get("channels").and_then(|c| c.as_array()) else {
                continue;
            };
            for ch in channels {
                let (Some(t), Some(url)) = (
                    ch.get("type").and_then(|v| v.as_str()),
                    ch.get("url").and_then(|v| v.as_str()),
                ) else {
                    continue;
                };
                let Ok(channel) = parse_channel_type(t) else {
                    continue;
                };
                let host = url
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .unwrap_or(t)
                    .to_string();
                out.push(ProbeTarget {
                    resource: format!("{app_name} / {pkg_name}"),
                    channel,
                    name: host,
                    url: url.to_string(),
                });
            }
        }
    }
    out
}

/// 构建矩阵：registry 缓存（新鲜）→ 网络交叉拉取 → 内置基线兜底。
/// `force=true` 强制重新拉 registry（「立即体检」不常用）。
pub fn build_targets(force: bool) -> Vec<ProbeTarget> {
    if let Ok(text) = registry::read_cache("index.json") {
        let fresh = registry::cache_fresh("index.json");
        if fresh || !force {
            if let Ok(index) = serde_json::from_str::<serde_json::Value>(&text) {
                let apps = collect_apps(&index);
                let rows = registry_targets(&index, &apps);
                if rows.len() > 3 {
                    return rows;
                }
            }
        }
    }
    // 网络交叉拉取（失败静默 → 基线）
    if let Ok((index, _)) = registry::fetch_index(force) {
        let apps = collect_apps(&index);
        let rows = registry_targets(&index, &apps);
        if rows.len() > 3 {
            return rows;
        }
    }
    baseline_targets()
}

/// 逐个 app 拉 manifest（尽力而为：单个失败跳过，不拖垮整个矩阵）。
fn collect_apps(index: &serde_json::Value) -> Vec<serde_json::Value> {
    let mut apps = Vec::new();
    let ids: Vec<String> = index
        .get("apps")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.get("id").and_then(|i| i.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();
    for id in ids {
        // 缓存优先，缺失再联网（fetch 内部含交叉校验）
        let doc = registry::read_cached_app(&id)
            .or_else(|_| registry::fetch_app(&id, false).map(|(v, _)| v));
        if let Ok(doc) = doc {
            apps.push(doc);
        }
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_covers_matrix_rows() {
        let rows = baseline_targets();
        assert!(rows.len() >= 10);
        // 关键资源全覆盖
        for res in [
            "registry JSON",
            "Node.js dist",
            "openclaw CLI (npm)",
            "Git for Windows",
            "OpenClaw Windows Hub",
        ] {
            assert!(rows.iter().any(|r| r.resource == res), "missing {res}");
        }
        // 通道类型分布：direct + mirror + proxy
        assert!(rows.iter().any(|r| r.channel == ChannelType::Direct));
        assert!(rows.iter().any(|r| r.channel == ChannelType::Mirror));
        assert!(rows.iter().any(|r| r.channel == ChannelType::Proxy));
    }

    #[test]
    fn registry_targets_expand_packages() {
        let index = serde_json::json!({"apps": [{"id": "openclaw", "name": "OpenClaw"}]});
        let app = serde_json::json!({
            "name": "OpenClaw",
            "packages": [{
                "id": "node-win-x64",
                "name": "Node.js Windows x64",
                "channels": [
                    {"type": "direct", "url": "https://nodejs.org/dist/v26.13.0/x.msi"},
                    {"type": "mirror", "url": "https://npmmirror.com/mirrors/node/v26.13.0/x.msi"},
                    {"type": "warp", "url": "https://bad"}
                ]
            }]
        });
        let rows = registry_targets(&index, std::slice::from_ref(&app));
        // 3 个 registry 源 + 2 个有效 package 通道（未知类型跳过）
        assert_eq!(rows.len(), 5);
        let pkg_row = rows
            .iter()
            .find(|r| r.resource.contains("Node.js"))
            .unwrap();
        assert_eq!(pkg_row.channel, ChannelType::Direct);
        assert_eq!(pkg_row.name, "nodejs.org");
    }

    #[test]
    fn build_targets_always_returns_rows() {
        // 离线环境（无缓存/网络失败）也必须回退到基线
        let rows = build_targets(false);
        assert!(!rows.is_empty());
    }
}
