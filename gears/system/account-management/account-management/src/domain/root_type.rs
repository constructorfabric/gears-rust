//! Account Management's concrete platform-root tenant-type contract.
//!
//! This configuration is independent from the optional root-tenant bootstrap
//! saga: deployments may reconcile the shared schema while creating the tenant
//! out of band.

use gts::GtsId;
use serde::Deserialize;
use toolkit_gts::gts_id;
use toolkit_macros::domain_model;

/// Abstract AM tenant-type envelope every concrete platform root derives from.
pub const TENANT_TYPE_BASE: &str = gts_id!("cf.core.am.tenant_type.v1~");

/// AM-owned semantic contract for the concrete platform-root type.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RootTypeConfig {
    /// Canonical concrete GTS type-schema identifier.
    pub gts_id: gts::GtsTypeId,
    /// Whether tenants of this type require dedicated `IdP` provisioning.
    pub idp_provisioning: bool,
}

impl Default for RootTypeConfig {
    fn default() -> Self {
        Self {
            gts_id: gts::GtsTypeId::new(""),
            idp_provisioning: false,
        }
    }
}

impl RootTypeConfig {
    /// Validate and return the canonical identifier.
    ///
    /// # Errors
    /// Returns a diagnostic when the ID is non-canonical, abstract, or not a
    /// direct child of the AM tenant-type envelope.
    pub fn validated_id(&self) -> Result<&str, String> {
        let type_id = self.gts_id.as_ref();
        let parsed = GtsId::try_new(type_id)
            .map_err(|error| format!("root_tenant_type.gts_id `{type_id}` is invalid: {error}"))?;
        if !parsed.is_type() || parsed.id() != type_id {
            return Err(format!(
                "root_tenant_type.gts_id `{type_id}` must be a canonical GTS type-schema ID"
            ));
        }
        let chain = parsed.chain_ids();
        if chain.len() != 2 || chain.first().map(String::as_str) != Some(TENANT_TYPE_BASE) {
            return Err(format!(
                "root_tenant_type.gts_id `{type_id}` must be a concrete type directly derived from {TENANT_TYPE_BASE}"
            ));
        }
        Ok(type_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_idp_provisioning_defaults_to_false() {
        let cfg: RootTypeConfig = serde_json::from_value(serde_json::json!({
            "gts_id": "gts.cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
        }))
        .expect("root-type config");
        assert!(!cfg.idp_provisioning);
    }

    #[test]
    fn accepts_concrete_am_tenant_type() {
        let cfg = RootTypeConfig {
            gts_id: gts::GtsTypeId::new(gts_id!(
                "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
            )),
            idp_provisioning: false,
        };
        assert!(cfg.validated_id().is_ok());
    }

    #[test]
    fn rejects_abstract_foreign_or_indirect_type() {
        for value in [
            TENANT_TYPE_BASE,
            gts_id!("cf.other.am.tenant_type.v1~cf.core.am.platform.v1~"),
            gts_id!("cf.core.am.tenant_type.v1~cf.core.am.intermediate.v1~cf.core.am.platform.v1~"),
        ] {
            let cfg = RootTypeConfig {
                gts_id: gts::GtsTypeId::new(value),
                idp_provisioning: false,
            };
            assert!(cfg.validated_id().is_err(), "must reject {value}");
        }
    }
}
