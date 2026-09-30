//! Account Management's concrete platform-root tenant-type contract.
//!
//! This configuration is independent from the optional root-tenant bootstrap
//! saga: deployments may register the shared schema while creating the tenant
//! out of band.

use gts::GtsId;
use serde::Deserialize;
use toolkit_gts::gts_id;
use toolkit_macros::domain_model;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::tenant::model::TenantModel;

/// Abstract AM tenant-type envelope every concrete platform root derives from.
pub const TENANT_TYPE_BASE: &str = gts_id!("cf.core.am.tenant_type.v1~");

/// Lifecycle states that can satisfy a configured platform-root binding.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootBindingLifecycle {
    /// Fully initialized root; bootstrap may skip idempotently.
    Active,
    /// Incomplete root that a validated bootstrap saga will resume.
    Provisioning,
}

/// AM-owned semantic contract for the concrete platform-root type.
#[domain_model]
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootTypeConfig {
    /// Canonical concrete GTS type-schema identifier.
    pub gts_id: gts::GtsTypeId,
    /// Whether tenants of this type require dedicated `IdP` provisioning.
    #[serde(default)]
    pub idp_provisioning: bool,
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

/// Validate an existing platform root against its create-once configuration.
///
/// `expected_root_id` is absent when bootstrap is disabled, but the durable
/// tenant-type binding is always checked whenever a root-type contract exists.
/// A `Provisioning` root is valid only while a validated bootstrap saga is
/// available to resume it.
///
/// # Errors
/// Returns [`DomainError::RootBindingMismatch`] when the root ID or type UUID
/// has drifted, and [`DomainError::InvalidTenantType`] for an invalid configured
/// GTS identifier.
pub fn validate_root_binding(
    existing: &TenantModel,
    expected_root_id: Option<Uuid>,
    bootstrap_will_run: bool,
    root_type: &RootTypeConfig,
) -> Result<RootBindingLifecycle, DomainError> {
    if let Some(expected_root_id) = expected_root_id
        && existing.id != expected_root_id
    {
        return Err(DomainError::RootBindingMismatch {
            detail: format!(
                "platform root already exists with id {}, but configured root_id is {expected_root_id}; an explicit root migration is required",
                existing.id
            ),
        });
    }

    let configured_type_uuid = GtsId::try_new(root_type.gts_id.as_ref())
        .map_err(|error| DomainError::InvalidTenantType {
            detail: format!(
                "invalid root_tenant_type.gts_id chain `{}`: {error}",
                root_type.gts_id
            ),
        })?
        .to_uuid();
    if existing.tenant_type_uuid != configured_type_uuid {
        return Err(DomainError::RootBindingMismatch {
            detail: format!(
                "platform root {} has tenant_type_uuid={}, but configured root_tenant_type.gts_id {} resolves to {configured_type_uuid}; an explicit root/schema migration is required",
                existing.id, existing.tenant_type_uuid, root_type.gts_id
            ),
        });
    }

    match existing.status {
        crate::domain::tenant::model::TenantStatus::Active => Ok(RootBindingLifecycle::Active),
        crate::domain::tenant::model::TenantStatus::Provisioning if bootstrap_will_run => {
            Ok(RootBindingLifecycle::Provisioning)
        }
        status => Err(DomainError::RootBindingMismatch {
            detail: format!(
                "platform root {} has lifecycle status `{}` which cannot satisfy the configured root binding; expected `active`{}; an explicit root recovery or migration is required",
                existing.id,
                status.as_str(),
                if bootstrap_will_run {
                    " or bootstrap-resumable `provisioning`"
                } else {
                    ""
                }
            ),
        }),
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
    fn omitted_gts_id_fails_deserialization() {
        let error = serde_json::from_value::<RootTypeConfig>(serde_json::json!({
            "idp_provisioning": false
        }))
        .expect_err("gts_id is required");

        assert!(
            error.to_string().contains("missing field `gts_id`"),
            "{error}"
        );
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

    fn root_model(
        id: Uuid,
        tenant_type_uuid: Uuid,
        status: crate::domain::tenant::model::TenantStatus,
    ) -> TenantModel {
        let now = time::OffsetDateTime::UNIX_EPOCH;
        TenantModel {
            id,
            parent_id: None,
            name: "root".to_owned(),
            status,
            self_managed: false,
            tenant_type_uuid,
            depth: 0,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    #[test]
    fn existing_root_binding_accepts_matching_id_and_type() {
        let cfg = RootTypeConfig {
            gts_id: gts::GtsTypeId::new(gts_id!(
                "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
            )),
            idp_provisioning: false,
        };
        let id = Uuid::from_u128(1);
        let type_uuid = GtsId::try_new(cfg.gts_id.as_ref())
            .expect("valid root type")
            .to_uuid();

        let lifecycle = validate_root_binding(
            &root_model(
                id,
                type_uuid,
                crate::domain::tenant::model::TenantStatus::Active,
            ),
            Some(id),
            false,
            &cfg,
        )
        .expect("matching durable binding");
        assert_eq!(lifecycle, RootBindingLifecycle::Active);
    }

    #[test]
    fn existing_root_binding_rejects_id_drift() {
        let cfg = RootTypeConfig {
            gts_id: gts::GtsTypeId::new(gts_id!(
                "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
            )),
            idp_provisioning: false,
        };
        let type_uuid = GtsId::try_new(cfg.gts_id.as_ref())
            .expect("valid root type")
            .to_uuid();
        let error = validate_root_binding(
            &root_model(
                Uuid::from_u128(2),
                type_uuid,
                crate::domain::tenant::model::TenantStatus::Active,
            ),
            Some(Uuid::from_u128(1)),
            true,
            &cfg,
        )
        .expect_err("root id drift must fail");

        assert!(
            matches!(error, DomainError::RootBindingMismatch { ref detail } if detail == "platform root already exists with id 00000000-0000-0000-0000-000000000002, but configured root_id is 00000000-0000-0000-0000-000000000001; an explicit root migration is required"),
            "{error:?}"
        );
    }

    #[test]
    fn existing_root_binding_rejects_type_drift_without_bootstrap() {
        let cfg = RootTypeConfig {
            gts_id: gts::GtsTypeId::new(gts_id!(
                "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
            )),
            idp_provisioning: false,
        };
        let id = Uuid::from_u128(1);
        let error = validate_root_binding(
            &root_model(
                id,
                Uuid::nil(),
                crate::domain::tenant::model::TenantStatus::Active,
            ),
            None,
            false,
            &cfg,
        )
        .expect_err("type drift must fail even without bootstrap");

        assert!(
            matches!(error, DomainError::RootBindingMismatch { ref detail } if detail.contains("explicit root/schema migration")),
            "{error:?}"
        );
    }

    #[test]
    fn provisioning_root_requires_runnable_bootstrap() {
        let cfg = RootTypeConfig {
            gts_id: gts::GtsTypeId::new(gts_id!(
                "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
            )),
            idp_provisioning: false,
        };
        let id = Uuid::from_u128(1);
        let type_uuid = GtsId::try_new(cfg.gts_id.as_ref())
            .expect("valid root type")
            .to_uuid();
        let provisioning = root_model(
            id,
            type_uuid,
            crate::domain::tenant::model::TenantStatus::Provisioning,
        );

        let lifecycle = validate_root_binding(&provisioning, Some(id), true, &cfg)
            .expect("runnable bootstrap must be allowed to resume provisioning");
        assert_eq!(lifecycle, RootBindingLifecycle::Provisioning);
        let error = validate_root_binding(&provisioning, None, false, &cfg)
            .expect_err("provisioning without runnable bootstrap must fail");
        assert!(
            matches!(error, DomainError::RootBindingMismatch { ref detail } if detail.contains("lifecycle status `provisioning`")),
            "{error:?}"
        );
    }

    #[test]
    fn terminal_root_lifecycle_states_are_binding_mismatches() {
        let cfg = RootTypeConfig {
            gts_id: gts::GtsTypeId::new(gts_id!(
                "cf.core.am.tenant_type.v1~cf.core.am.platform.v1~"
            )),
            idp_provisioning: false,
        };
        let id = Uuid::from_u128(1);
        let type_uuid = GtsId::try_new(cfg.gts_id.as_ref())
            .expect("valid root type")
            .to_uuid();

        for status in [
            crate::domain::tenant::model::TenantStatus::Suspended,
            crate::domain::tenant::model::TenantStatus::Deleted,
        ] {
            let error =
                validate_root_binding(&root_model(id, type_uuid, status), Some(id), true, &cfg)
                    .expect_err("terminal root lifecycle state must fail");
            assert!(
                matches!(error, DomainError::RootBindingMismatch { ref detail } if detail.contains(status.as_str())),
                "{error:?}"
            );
        }
    }
}
