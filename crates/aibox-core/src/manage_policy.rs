//! Verified registry policy for maintenance IPC. Caller lists are confirmation only.

use serde::Deserialize;
use serde_json::Value;

use crate::err::{CoreError, CoreResult};
use crate::registry;

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MaintenancePolicy {
    pub data_paths: Vec<String>,
    pub uninstall: UninstallPolicy,
    #[serde(rename = "risk")]
    _risk: RiskPolicy,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct UninstallPolicy {
    pub residual_paths: Vec<String>,
    pub steps: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RiskPolicy {
    #[serde(rename = "backupFirst")]
    _backup_first: bool,
}

impl Default for RiskPolicy {
    fn default() -> Self {
        Self {
            _backup_first: true,
        }
    }
}

impl MaintenancePolicy {
    pub fn fetch(app_id: &str) -> CoreResult<Self> {
        registry::validate_id(app_id)?;
        let (doc, source) = registry::fetch_app(app_id, true).map_err(|e| {
            if registry::is_availability_err(&e) {
                CoreError::new("MANAGE_REGISTRY_UNVERIFIED", e.human)
            } else {
                e
            }
        })?;
        Self::from_verified_doc(app_id, &doc, &source)
    }

    fn from_verified_doc(app_id: &str, doc: &Value, source: &str) -> CoreResult<Self> {
        registry::validate_id(app_id)?;
        if !registry::is_network_verified_source(source) {
            return Err(CoreError::new(
                "MANAGE_REGISTRY_UNVERIFIED",
                "Maintenance requires a verified network registry",
            ));
        }
        if doc.get("id").and_then(Value::as_str) != Some(app_id) {
            return Err(invalid_policy(
                "Registry app identity does not match the requested app",
            ));
        }
        let Some(maintenance) = doc.get("maintenance") else {
            return Ok(Self::default());
        };
        if !maintenance.is_object()
            || ["uninstall", "risk"]
                .iter()
                .any(|key| maintenance.get(key).is_some_and(|v| !v.is_object()))
        {
            return Err(invalid_policy("Malformed maintenance policy"));
        }
        let policy: Self = serde_json::from_value(maintenance.clone())
            .map_err(|_| invalid_policy("Malformed maintenance policy"))?;
        for field in [
            &policy.data_paths,
            &policy.uninstall.residual_paths,
            &policy.uninstall.steps,
        ] {
            if field
                .iter()
                .any(|s| s.trim().is_empty() || s.contains('\0'))
            {
                return Err(invalid_policy("Maintenance lists contain invalid entries"));
            }
        }
        Ok(policy)
    }

    pub fn confirm_backup(&self, data_paths: &[String]) -> CoreResult<()> {
        compare(data_paths, &self.data_paths)
    }

    pub fn confirm_remove(
        &self,
        residual_paths: &[String],
        data_paths: &[String],
        steps: &[String],
        backup: Option<bool>,
    ) -> CoreResult<bool> {
        compare(data_paths, &self.data_paths)?;
        compare(residual_paths, &self.uninstall.residual_paths)?;
        compare(steps, &self.uninstall.steps)?;
        if self.data_paths.is_empty()
            && self.uninstall.residual_paths.is_empty()
            && self.uninstall.steps.is_empty()
        {
            return Err(CoreError::new(
                "MANAGE_NO_MANIFEST",
                "The app declares no maintenance actions",
            ));
        }
        // Preserve the IPC default: only an explicit UI choice can opt out.
        Ok(backup.unwrap_or(true))
    }
}

fn invalid_policy(message: &str) -> CoreError {
    CoreError::new("MANAGE_POLICY_INVALID", message)
}

fn compare(confirmed: &[String], declared: &[String]) -> CoreResult<()> {
    // Exact strings and multiplicity. Order does not change user consent; execution
    // still uses only the authoritative lists, in registry order.
    let sorted = |list: &[String]| {
        let mut copy = list.to_vec();
        copy.sort_unstable();
        copy
    };
    if sorted(confirmed) != sorted(declared) {
        return Err(CoreError::new(
            "MANAGE_POLICY_CHANGED",
            "The confirmed maintenance list does not match the verified registry",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn policy_requires_network_and_matching_app() {
        let doc = json!({"id":"fixture", "maintenance":{"dataPaths":["~/fixture"]}});
        for source in ["cache", "cache-stale", "snapshot", "other"] {
            assert_eq!(
                MaintenancePolicy::from_verified_doc("fixture", &doc, source)
                    .unwrap_err()
                    .code,
                "MANAGE_REGISTRY_UNVERIFIED"
            );
        }
        assert!(MaintenancePolicy::from_verified_doc("fixture", &doc, "raw").is_ok());
        assert!(MaintenancePolicy::from_verified_doc("different", &doc, "raw").is_err());
        assert!(MaintenancePolicy::from_verified_doc("", &doc, "raw").is_err());
    }

    #[test]
    fn malformed_policy_never_becomes_an_empty_or_partial_list() {
        for maintenance in [
            json!(null),
            json!([]),
            json!({"uninstall":[]}),
            json!({"risk":[]}),
            json!({"dataPaths":["~/fixture", 7]}),
            json!({"dataPaths":[" "]}),
            json!({"uninstall":null}),
            json!({"uninstall":{"residualPaths":null}}),
            json!({"uninstall":{"steps":"manual"}}),
            json!({"risk":{"backupFirst":"false"}}),
        ] {
            let doc = json!({"id":"fixture", "maintenance":maintenance});
            assert_eq!(
                MaintenancePolicy::from_verified_doc("fixture", &doc, "raw")
                    .unwrap_err()
                    .code,
                "MANAGE_POLICY_INVALID"
            );
        }
    }

    #[test]
    fn compare_every_confirmed_field() {
        let doc = json!({"id":"fixture", "maintenance":{
            "dataPaths":["~/data", "~/second"],
            "uninstall":{"residualPaths":["~/fixture"],"steps":["manual"]}
        }});
        let policy = MaintenancePolicy::from_verified_doc("fixture", &doc, "raw").unwrap();
        let data = ["~/second".into(), "~/data".into()];
        let residual = ["~/fixture".into()];
        let steps = ["manual".into()];
        assert!(policy
            .confirm_remove(&residual, &data, &steps, None)
            .unwrap());
        for (r, d, s) in [
            (&[][..], &data[..], &steps[..]),
            (&residual[..], &data[..1], &steps[..]),
            (&residual[..], &data[..], &[][..]),
        ] {
            assert_eq!(
                policy
                    .confirm_remove(r, d, s, Some(false))
                    .unwrap_err()
                    .code,
                "MANAGE_POLICY_CHANGED"
            );
        }
        assert_eq!(
            policy.confirm_backup(&[]).unwrap_err().code,
            "MANAGE_POLICY_CHANGED"
        );
    }
}
