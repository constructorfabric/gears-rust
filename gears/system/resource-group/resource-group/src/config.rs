//! Configuration for the resource-group gear.

use resource_group_sdk::TENANT_RG_TYPE_PATH;
use serde::Deserialize;

use crate::domain::validation;

/// Gear configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceGroupConfig {
    /// GTS type-path prefixes that classify an RG type as a tenant type.
    ///
    /// A type whose code starts with any of these opens a new tenant scope
    /// (`tenant_id = group.id`) and takes part in the tenant-root uniqueness
    /// check. Defaults to `[TENANT_RG_TYPE_PATH]`; a deployment with its own
    /// vendor sets e.g. `["gts.cf.core.rg.type.v1~acme.core._.tenant.v1~"]`.
    pub tenant_type_paths: Vec<String>,
}

impl Default for ResourceGroupConfig {
    fn default() -> Self {
        Self {
            tenant_type_paths: vec![TENANT_RG_TYPE_PATH.to_owned()],
        }
    }
}

impl ResourceGroupConfig {
    /// Reject an empty list, non-canonical entries, and paths outside the RG
    /// type namespace.
    ///
    /// Entries must already be canonical (trimmed, lowercase): tenant
    /// classification is a case-sensitive `starts_with` against stored type
    /// codes, so a non-canonical prefix would silently match nothing.
    ///
    /// # Errors
    ///
    /// Returns an error describing the first invalid entry.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.tenant_type_paths.is_empty() {
            anyhow::bail!("resource-group: tenant_type_paths must not be empty");
        }
        for path in &self.tenant_type_paths {
            if path.trim() != path || path.to_lowercase() != *path {
                anyhow::bail!(
                    "resource-group: tenant_type_paths entry '{path}' must be trimmed and lowercase"
                );
            }
            validation::validate_type_code(path).map_err(|e| {
                anyhow::anyhow!("resource-group: invalid tenant_type_paths entry '{path}': {e}")
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn default_is_sdk_tenant_type() {
        let cfg = ResourceGroupConfig::default();
        assert_eq!(cfg.tenant_type_paths, vec![TENANT_RG_TYPE_PATH.to_owned()]);
        cfg.validate().unwrap();
    }

    #[test]
    fn empty_object_falls_back_to_default() {
        let cfg: ResourceGroupConfig = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(cfg.tenant_type_paths, vec![TENANT_RG_TYPE_PATH.to_owned()]);
    }

    #[test]
    fn custom_tenant_types_are_accepted() {
        let cfg: ResourceGroupConfig = serde_json::from_value(serde_json::json!({
            "tenant_type_paths": [
                "gts.cf.core.rg.type.v1~acme.core._.tenant.v1~",
                TENANT_RG_TYPE_PATH,
            ]
        }))
        .unwrap();
        assert_eq!(cfg.tenant_type_paths.len(), 2);
        cfg.validate().unwrap();
    }

    #[test]
    fn empty_list_is_rejected() {
        let cfg = ResourceGroupConfig {
            tenant_type_paths: Vec::new(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn path_outside_rg_namespace_is_rejected() {
        let cfg = ResourceGroupConfig {
            tenant_type_paths: vec!["gts.acme.core._.tenant.v1~".to_owned()],
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn non_canonical_path_is_rejected() {
        for path in [
            format!(" {TENANT_RG_TYPE_PATH}"),
            format!("{TENANT_RG_TYPE_PATH} "),
            TENANT_RG_TYPE_PATH.to_uppercase(),
        ] {
            let cfg = ResourceGroupConfig {
                tenant_type_paths: vec![path.clone()],
            };
            assert!(cfg.validate().is_err(), "{path:?} must be rejected");
        }
    }

    #[test]
    fn unknown_field_is_rejected() {
        let res: Result<ResourceGroupConfig, _> =
            serde_json::from_value(serde_json::json!({ "tenant_type": "x" }));
        assert!(res.is_err());
    }
}
