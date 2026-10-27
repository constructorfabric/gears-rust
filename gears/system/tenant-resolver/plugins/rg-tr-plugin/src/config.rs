//! Configuration for the RG tenant resolver plugin.

use resource_group_sdk::{GtsTypePath, TENANT_RG_TYPE_PATH, TYPE_RESOURCE_TYPE};
use serde::Deserialize;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RgTrPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,

    /// Exact RG type paths whose groups are treated as tenants (matched with
    /// `type in (...)`). Defaults to `[TENANT_RG_TYPE_PATH]`.
    ///
    /// Unlike resource-group's `tenant_type_paths` (prefixes), these are
    /// concrete types: list every tenant type the deployment registers. Each
    /// entry must be registered in resource-group — RG rejects a `$filter`
    /// that names an unknown type.
    pub tenant_types: Vec<String>,
}

impl Default for RgTrPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            priority: 50,
            tenant_types: vec![TENANT_RG_TYPE_PATH.to_owned()],
        }
    }
}

impl RgTrPluginConfig {
    /// Reject an empty `tenant_types` list and malformed entries.
    ///
    /// Entries must be canonical (trimmed, lowercase) RG type paths: RG
    /// matches `type` filter values against stored type codes verbatim, and
    /// rejects the whole `$filter` when one value is not a valid type path —
    /// failing every lookup instead of failing once at init.
    ///
    /// # Errors
    ///
    /// Returns an error describing the first invalid entry.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.tenant_types.is_empty() {
            anyhow::bail!("rg-tr-plugin: tenant_types must not be empty");
        }
        for t in &self.tenant_types {
            if t.is_empty() || t.trim() != t || t.to_lowercase() != *t {
                anyhow::bail!(
                    "rg-tr-plugin: tenant_types entry '{t}' must be non-empty, trimmed and lowercase"
                );
            }
            if !t.starts_with(TYPE_RESOURCE_TYPE) {
                anyhow::bail!(
                    "rg-tr-plugin: tenant_types entry '{t}' must start with '{TYPE_RESOURCE_TYPE}'"
                );
            }
            GtsTypePath::new(t.as_str()).map_err(|e| {
                anyhow::anyhow!("rg-tr-plugin: invalid tenant_types entry '{t}': {e}")
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
    fn default_is_valid() {
        RgTrPluginConfig::default().validate().unwrap();
    }

    #[test]
    fn empty_list_is_rejected() {
        let cfg = RgTrPluginConfig {
            tenant_types: Vec::new(),
            ..RgTrPluginConfig::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn non_canonical_entry_is_rejected() {
        for t in [
            format!(" {TENANT_RG_TYPE_PATH}"),
            TENANT_RG_TYPE_PATH.to_uppercase(),
            String::new(),
            "not-a-gts-type".to_owned(),
            format!("{TYPE_RESOURCE_TYPE}not-a-gts-type"),
            // Valid GTS id, but outside the RG type namespace.
            toolkit_gts::gts_id!("acme.core._.tenant.v1~").to_owned(),
        ] {
            let cfg = RgTrPluginConfig {
                tenant_types: vec![t.clone()],
                ..RgTrPluginConfig::default()
            };
            assert!(cfg.validate().is_err(), "{t:?} must be rejected");
        }
    }
}
