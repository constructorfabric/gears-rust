//! Configuration for the static tenant resolver plugin.

use anyhow::{Context, bail};
use serde::Deserialize;
use tenant_resolver_sdk::TenantStatus;
use uuid::Uuid;

/// Plugin configuration.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StaticTrPluginConfig {
    /// Vendor name for GTS instance registration.
    pub vendor: String,

    /// Plugin priority (lower = higher priority).
    pub priority: i16,

    /// Static tenant definitions.
    pub tenants: Vec<TenantConfig>,
}

impl Default for StaticTrPluginConfig {
    fn default() -> Self {
        Self {
            vendor: "cyberfabric".to_owned(),
            priority: 100,
            tenants: Vec::new(),
        }
    }
}

impl StaticTrPluginConfig {
    /// Validates the single-root tree topology invariant.
    ///
    /// Enforces that the configured tenants form a single-root tree:
    /// - exactly one tenant has `parent_id == None`;
    /// - tenant ids are unique;
    /// - no tenant references itself via `parent_id`;
    /// - every referenced `parent_id` belongs to a configured tenant;
    /// - every tenant's parent chain reaches the unique root (no cycles and
    ///   no disconnected components).
    ///
    /// # Errors
    ///
    /// Returns an error if any of the invariants above are violated.
    pub fn validate(&self) -> anyhow::Result<()> {
        let roots: Vec<Uuid> = self
            .tenants
            .iter()
            .filter(|t| t.parent_id.is_none())
            .map(|t| t.id)
            .collect();

        let root_id = match roots.len() {
            0 => bail!(
                "static-tr-plugin: no root tenant configured -- exactly one tenant must have \
                 no parent_id (single-root tree topology)"
            ),
            1 => roots[0],
            n => bail!(
                "static-tr-plugin: {n} root tenants configured ({roots:?}) -- exactly one \
                 tenant must have no parent_id (single-root tree topology)"
            ),
        };

        let mut parent_by_id: std::collections::HashMap<Uuid, Option<Uuid>> =
            std::collections::HashMap::with_capacity(self.tenants.len());
        for tenant in &self.tenants {
            if parent_by_id.insert(tenant.id, tenant.parent_id).is_some() {
                return Err(anyhow::anyhow!(
                    "static-tr-plugin: duplicate tenant id {} in configuration",
                    tenant.id,
                ))
                .context("invalid tenant hierarchy configuration");
            }
        }

        for tenant in &self.tenants {
            let Some(parent_id) = tenant.parent_id else {
                continue;
            };
            if parent_id == tenant.id {
                return Err(anyhow::anyhow!(
                    "static-tr-plugin: tenant {} lists itself as parent_id",
                    tenant.id,
                ))
                .context("invalid tenant hierarchy configuration");
            }
            if !parent_by_id.contains_key(&parent_id) {
                return Err(anyhow::anyhow!(
                    "static-tr-plugin: tenant {} references parent_id {parent_id} which is not \
                     in the configured tenants",
                    tenant.id,
                ))
                .context("invalid tenant hierarchy configuration");
            }
        }

        // Every tenant must be reachable from the unique root by walking
        // parent_id. Catches cycles among non-root tenants (e.g. A->B, B->A)
        // that the checks above let through.
        for tenant in &self.tenants {
            let mut seen = std::collections::HashSet::new();
            let mut current = tenant.id;
            while current != root_id {
                if !seen.insert(current) {
                    return Err(anyhow::anyhow!(
                        "static-tr-plugin: tenant {} is in a parent_id cycle and does not \
                         descend from root {}",
                        tenant.id,
                        root_id,
                    ))
                    .context("invalid tenant hierarchy configuration");
                }
                // parent_by_id[current] is Some for every non-root tenant at
                // this point (unique-root + dangling-parent checks above);
                // the None arm is defensive.
                match parent_by_id.get(&current).copied().flatten() {
                    Some(parent_id) => current = parent_id,
                    None => {
                        return Err(anyhow::anyhow!(
                            "static-tr-plugin: tenant {} does not descend from root {}",
                            tenant.id,
                            root_id,
                        ))
                        .context("invalid tenant hierarchy configuration");
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "config_tests.rs"]
mod config_tests;

/// Configuration for a single tenant.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TenantConfig {
    /// Tenant ID.
    pub id: Uuid,

    /// Tenant name.
    pub name: String,

    /// Tenant status (defaults to Active).
    #[serde(default)]
    pub status: TenantStatus,

    /// Tenant type classification.
    #[serde(rename = "type", default)]
    pub tenant_type: Option<String>,

    /// Parent tenant ID. `None` for the root tenant. Exactly one configured
    /// tenant is expected to have `parent_id == None` (single-root tree).
    #[serde(default)]
    pub parent_id: Option<Uuid>,

    /// Whether this tenant is self-managed (barrier).
    /// When `true`, parent tenants cannot traverse into this subtree
    /// unless `BarrierMode::Ignore` is used.
    #[serde(default)]
    pub self_managed: bool,
}
