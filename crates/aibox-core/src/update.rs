//! `update_check`：GitHub releases/latest → 有新版黄条。
//! **只查不替换**（便携升级：跳官网下载，不做自替换）。5min 缓存由壳侧持有。

use serde::Serialize;

use crate::err::{CoreError, CoreResult};

/// UpdateInfo。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub has_update: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

const LATEST_URL: &str = "https://api.github.com/repos/imAiBOX/aibox-app/releases/latest";

/// 版本比较（`v` 前缀可选；数字段逐段比，非数字段字典序）。`candidate > current`。
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn segs(s: &str) -> Vec<String> {
        s.trim()
            .trim_start_matches('v')
            .split(['.', '-', '+'])
            .map(|p| p.to_string())
            .collect()
    }
    let a = segs(candidate);
    let b = segs(current);
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).map(String::as_str).unwrap_or("0");
        let y = b.get(i).map(String::as_str).unwrap_or("0");
        let (xn, yn) = (
            x.chars().all(|c| c.is_ascii_digit()) && !x.is_empty(),
            y.chars().all(|c| c.is_ascii_digit()) && !y.is_empty(),
        );
        match (xn, yn) {
            (true, true) => {
                let (xn, yn) = (
                    x.parse::<u128>().unwrap_or(0),
                    y.parse::<u128>().unwrap_or(0),
                );
                if xn != yn {
                    return xn > yn;
                }
            }
            _ => {
                if x != y {
                    return x > y;
                }
            }
        }
    }
    false
}

/// 拉 releases/latest（同步；调用方放入 spawn_blocking）。
pub fn check_update(current_version: &str) -> CoreResult<UpdateInfo> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(10))
        .timeout_connect(std::time::Duration::from_secs(5))
        .user_agent("aibox-app/0.0.1")
        .build();
    let body = match agent
        .get(LATEST_URL)
        .set("Accept", "application/vnd.github+json")
        .call()
    {
        Ok(r) => r.into_string().map_err(|e| {
            CoreError::new("UPDATE_BAD_RESPONSE", format!("Read release body: {e}"))
        })?,
        Err(ureq::Error::Status(code, _)) => {
            return Err(CoreError::new(
                "UPDATE_HTTP_ERROR",
                format!("GitHub releases/latest returned HTTP {code}"),
            ))
        }
        Err(ureq::Error::Transport(t)) => {
            return Err(CoreError::new(
                "UPDATE_NET_FAIL",
                format!("Could not reach GitHub releases: {t}"),
            ))
        }
    };

    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| CoreError::new("UPDATE_BAD_RESPONSE", format!("Release JSON invalid: {e}")))?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .unwrap_or_default()
        .to_string();
    if tag.is_empty() {
        return Err(CoreError::new(
            "UPDATE_BAD_RESPONSE",
            "Latest release has no tag_name",
        ));
    }
    let url = v.get("html_url").and_then(|t| t.as_str()).map(String::from);

    let has_update = is_newer(&tag, current_version);
    Ok(UpdateInfo {
        has_update,
        version: Some(tag),
        url: if has_update { url } else { None },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_compare() {
        assert!(is_newer("v0.0.2", "0.0.1"));
        assert!(is_newer("0.1.0", "0.0.9"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.0.1", "0.0.1"), "同版本不算新");
        assert!(!is_newer("0.0.1", "0.0.2"));
        assert!(is_newer("2026.11.2", "2026.11.1"));
        assert!(!is_newer("0.0.1", "0.0.2-beta"), "短列表补 0 段");
    }

    #[test]
    fn update_info_serializes_camel_case() {
        let info = UpdateInfo {
            has_update: true,
            version: Some("v0.0.2".into()),
            url: Some("https://example".into()),
        };
        let v = serde_json::to_value(info).unwrap();
        assert_eq!(v["hasUpdate"], true);
        assert_eq!(v["version"], "v0.0.2");
        assert!(v.get("url").is_some());
    }
}
