//! Configuration for the quota resolver gear.

use std::collections::BTreeMap;

use quota_resolver_sdk::{QUOTA_SUBJECT_TYPE_ID, QUOTA_TENANT_SUBJECT_TYPE_ID};
use serde::Deserialize;

/// Gear configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QuotaResolverConfig {
    /// Vendor selector used to pick a backend plugin implementation; the plugin
    /// with the lowest priority among that vendor's instances wins.
    pub vendor: String,
    /// Principal type id to quota subject type id. A quota subject type is
    /// derived from the quota subject base and is neither the base nor the
    /// tenant subject type.
    pub principal_subject_types: BTreeMap<String, String>,
}

impl Default for QuotaResolverConfig {
    fn default() -> Self {
        Self {
            vendor: "constructorfabric".to_owned(),
            principal_subject_types: BTreeMap::new(),
        }
    }
}

impl QuotaResolverConfig {
    /// Check the principal mapping.
    ///
    /// # Errors
    ///
    /// A key or a value is not a GTS type id, a value is not derived from the
    /// quota subject base, or a value is the base or the tenant subject type.
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.vendor.trim().is_empty(), "vendor must not be empty");
        for (principal, subject) in &self.principal_subject_types {
            gts::GtsTypeId::try_new(principal).map_err(|e| {
                anyhow::anyhow!(
                    "principal_subject_types key `{principal}` is not a GTS type id: {e}"
                )
            })?;
            gts::GtsTypeId::try_new(subject).map_err(|e| {
                anyhow::anyhow!(
                    "principal_subject_types value `{subject}` is not a GTS type id: {e}"
                )
            })?;
            anyhow::ensure!(
                subject.starts_with(QUOTA_SUBJECT_TYPE_ID),
                "principal_subject_types value `{subject}` is not derived from `{QUOTA_SUBJECT_TYPE_ID}`"
            );
            anyhow::ensure!(
                subject != QUOTA_SUBJECT_TYPE_ID && subject != QUOTA_TENANT_SUBJECT_TYPE_ID,
                "principal_subject_types value `{subject}` is the quota subject base or the tenant subject type"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;
