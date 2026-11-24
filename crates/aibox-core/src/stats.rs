//! Desktop stats are disabled. Keep the legacy IPC payload and success responses
//! without contacting the stats service or reading/writing a device identifier.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::err::CoreResult;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStat {
    pub views: u64,
    pub installs: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsData {
    pub apps: BTreeMap<String, AppStat>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

pub fn fetch_stats() -> CoreResult<StatsData> {
    Ok(StatsData::default())
}

pub fn track_install(_app_id: &str) -> CoreResult<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_stats_keep_legacy_empty_payload() {
        let data = fetch_stats().unwrap();
        assert_eq!(
            serde_json::to_value(data).unwrap(),
            serde_json::json!({ "apps": {} })
        );
    }

    #[test]
    fn disabled_tracking_accepts_legacy_callers() {
        for id in ["openclaw", "", "   "] {
            assert!(track_install(id).is_ok());
        }
    }
}
