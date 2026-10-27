//! Configuration for the RG tenant resolver plugin.

use resource_group_sdk::TENANT_RG_TYPE_PATH;
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
    /// Reject an empty `tenant_types` list and non-canonical entries.
    ///
    /// Entries must be trimmed and lowercase: RG matches `type` filter values
    /// against stored type codes verbatim.
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
        ] {
            let cfg = RgTrPluginConfig {
                tenant_types: vec![t.clone()],
                ..RgTrPluginConfig::default()
            };
            assert!(cfg.validate().is_err(), "{t:?} must be rejected");
        }
    }
}
