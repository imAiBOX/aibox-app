//! 应用统计（views/installs）：`aibox.im/api/stats` 拉取 + 安装上报。
//!
//! 两条链路都 **best-effort**：网络失败转结构化 Err，由调用方静默吞掉，
//! 绝不阻塞安装主流程。计数展示端（S1 卡片）拿不到数据就不显示。

use std::collections::BTreeMap;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::err::{CoreError, CoreResult};

const STATS_URL: &str = "https://aibox.im/api/stats";
const UA: &str = "aibox-app/0.0.1";
/// 上报/拉取统一 5s 超时（慢网络宁可放弃，不拖流程）。
const TIMEOUT: Duration = Duration::from_secs(5);

/// 单应用计数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStat {
    pub views: u64,
    pub installs: u64,
}

/// `GET /api/stats` 返回体：`{apps: {"<id>": {views, installs}}, updatedAt}`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsData {
    pub apps: BTreeMap<String, AppStat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(TIMEOUT)
        .timeout_connect(TIMEOUT)
        .user_agent(UA)
        .build()
}

/// 解析 `GET /api/stats` 响应（纯函数，容错）：
/// - 顶层必须是 JSON 对象（否则 Err）；缺 `apps` → 空表（不显示即可）；
/// - 单条非对象 / views·installs 非数字 → 该条跳过，不拖垮其余计数。
pub fn parse_stats(text: &str) -> CoreResult<StatsData> {
    let v: Value = serde_json::from_str(text)
        .map_err(|e| CoreError::new("STATS_BAD_RESPONSE", format!("Stats JSON invalid: {e}")))?;
    let obj = v
        .as_object()
        .ok_or_else(|| CoreError::new("STATS_BAD_RESPONSE", "Stats JSON is not an object"))?;
    let mut apps = BTreeMap::new();
    if let Some(entries) = obj.get("apps").and_then(|a| a.as_object()) {
        for (id, val) in entries {
            let Some(o) = val.as_object() else { continue };
            let num = |k: &str| o.get(k).and_then(|x| x.as_u64());
            if num("views").is_none() && num("installs").is_none() {
                continue; // 两个计数都不是数字 → 跳过该条
            }
            apps.insert(
                id.clone(),
                AppStat {
                    views: num("views").unwrap_or(0),
                    installs: num("installs").unwrap_or(0),
                },
            );
        }
    }
    let updated_at = obj.get("updatedAt").and_then(|x| x.as_str()).map(String::from);
    Ok(StatsData { apps, updated_at })
}

/// 拉统计（同步；调用方放入 spawn_blocking）。失败 → Err（前端不显示）。
pub fn fetch_stats() -> CoreResult<StatsData> {
    match agent().get(STATS_URL).call() {
        Ok(r) => {
            let text = r
                .into_string()
                .map_err(|e| CoreError::new("STATS_BAD_RESPONSE", format!("Read stats body: {e}")))?;
            parse_stats(&text)
        }
        Err(ureq::Error::Status(code, _)) => Err(CoreError::new(
            "STATS_HTTP_ERROR",
            format!("Stats endpoint returned HTTP {code}"),
        )),
        Err(ureq::Error::Transport(t)) => Err(CoreError::new(
            "STATS_NET_FAIL",
            format!("Could not reach stats endpoint: {t}"),
        )),
    }
}

/// 上报请求体（纯函数）：`{appId, type: "install"}`。
pub fn track_body(app_id: &str) -> Value {
    serde_json::json!({ "appId": app_id, "type": "install" })
}

/// 安装上报（同步；调用方放入 spawn_blocking）。
/// 服务端负责白名单 + 去重兜底；客户端只在任务队列启动时点调一次。
/// 失败 → Err（调用方静默吞，绝不阻塞安装流）。
pub fn track_install(app_id: &str) -> CoreResult<()> {
    let id = app_id.trim();
    if id.is_empty() {
        return Err(CoreError::new("STATS_BAD_APP_ID", "Empty appId"));
    }
    let body = track_body(id).to_string();
    match agent()
        .post(STATS_URL)
        .set("Content-Type", "application/json")
        .send_string(&body)
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, _)) => Err(CoreError::new(
            "STATS_HTTP_ERROR",
            format!("Install tracking returned HTTP {code}"),
        )),
        Err(ureq::Error::Transport(t)) => Err(CoreError::new(
            "STATS_NET_FAIL",
            format!("Could not reach stats endpoint: {t}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_payload() {
        let d = parse_stats(r#"{"apps":{"openclaw":{"views":1240,"installs":318}},"updatedAt":"2026-11-14T00:00:00Z"}"#).unwrap();
        assert_eq!(d.apps["openclaw"].views, 1240);
        assert_eq!(d.apps["openclaw"].installs, 318);
        assert_eq!(d.updated_at.as_deref(), Some("2026-11-14T00:00:00Z"));
    }

    #[test]
    fn parse_tolerates_missing_or_junk_entries() {
        // 缺 apps → 空表
        let d = parse_stats("{}").unwrap();
        assert!(d.apps.is_empty());
        // 单条非对象 / 计数非数字 → 跳过该条，其余保留；部分坏值 → 该位归 0
        let d = parse_stats(
            r#"{"apps":{"good":{"views":7,"installs":1},"bad":"oops","mixed":{"views":"x","installs":5}}}"#,
        )
        .unwrap();
        assert_eq!(d.apps.len(), 2);
        assert_eq!(d.apps["good"].views, 7);
        assert_eq!(d.apps["mixed"].views, 0);
        assert_eq!(d.apps["mixed"].installs, 5);
    }

    #[test]
    fn parse_rejects_malformed_json() {
        assert!(parse_stats("not json").is_err());
        assert!(parse_stats("[]").is_err());
    }

    #[test]
    fn track_body_shape() {
        let b = track_body("openclaw");
        assert_eq!(b["appId"], "openclaw");
        assert_eq!(b["type"], "install");
        assert_eq!(b.as_object().unwrap().len(), 2);
    }

    #[test]
    fn track_install_rejects_empty_id_without_network() {
        // 空 id → 本地直接 Err（不发请求，离线可测）
        assert!(track_install("").is_err());
        assert!(track_install("   ").is_err());
        assert_eq!(track_install("").unwrap_err().code, "STATS_BAD_APP_ID");
    }

    #[test]
    fn stats_data_serializes_camel_case() {
        let mut apps = BTreeMap::new();
        apps.insert("a".to_string(), AppStat { views: 1, installs: 2 });
        let v = serde_json::to_value(StatsData {
            apps,
            updated_at: Some("t".into()),
        })
        .unwrap();
        assert_eq!(v["apps"]["a"]["views"], 1);
        assert_eq!(v["updatedAt"], "t");
    }
}
