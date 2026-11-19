//! [`RgTypeCatalog`] over the types registry's tenant API (`TypesRegistryApi`).
//!
//! Every read carries the caller's `SecurityContext`. Reads go through the
//! public SDK reads and `ReadOptions`; there is no adapter cache or local
//! catalogue (the SDK client cache, T36, is a separate fleet gate).

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, GtsTypeId, LifecycleStatus, Projection, ReadOptions,
    TypesRegistryApi, TypesRegistryApiExt,
};

use crate::domain::error::DomainError;
use crate::domain::type_catalog::{RgType, RgTypeCatalog, TypeUse};

/// Production [`RgTypeCatalog`].
// @cpt-dod:cpt-cf-resource-group-dod-type-mgmt-registry-catalog:p1
pub struct RegistryTypeCatalog {
    registry: Arc<dyn TypesRegistryApi>,
}

impl RegistryTypeCatalog {
    #[must_use]
    pub fn new(registry: Arc<dyn TypesRegistryApi>) -> Self {
        Self { registry }
    }
}

fn registry_failure(error: CanonicalError, code: &str) -> DomainError {
    match error {
        CanonicalError::NotFound { .. } => DomainError::type_not_found(code),
        CanonicalError::Unauthenticated { .. } | CanonicalError::PermissionDenied { .. } => {
            DomainError::RegistryRefusedCaller(error)
        }
        CanonicalError::ServiceUnavailable { .. }
        | CanonicalError::DeadlineExceeded { .. }
        | CanonicalError::ResourceExhausted { .. }
        | CanonicalError::Aborted { .. } => DomainError::ServiceUnavailable {
            message: format!("types-registry read of '{code}' failed: {error}"),
        },
        other => {
            tracing::error!(%code, error = %other, "types-registry read failed");
            DomainError::InternalError
        }
    }
}

#[async_trait]
impl RgTypeCatalog for RegistryTypeCatalog {
    async fn get_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
        usage: TypeUse,
    ) -> Result<RgType, DomainError> {
        let type_id = GtsTypeId::try_new(code)
            .map_err(|e| DomainError::validation(format!("Invalid GTS type '{code}': {e}")))?;
        let schema = self
            .registry
            .get_type_schema(
                ctx,
                &type_id,
                ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(
                    &[
                        EntityField::Content,
                        EntityField::ResolvedSchema,
                        EntityField::EffectiveTraits,
                    ],
                ))),
            )
            .await
            .map_err(|e| registry_failure(e, code))?;
        if usage == TypeUse::NewLink && schema.lifecycle_status != LifecycleStatus::Active {
            return Err(DomainError::type_not_found(code));
        }
        RgType::from_schema(schema)
    }

    async fn get_membership_rule(
        &self,
        ctx: &SecurityContext,
        group_code: &str,
        member_code: &str,
    ) -> Result<RgType, DomainError> {
        let group_id = GtsTypeId::try_new(group_code).map_err(|e| {
            DomainError::validation(format!("Invalid GTS type '{group_code}': {e}"))
        })?;
        let member_id = GtsTypeId::try_new(member_code).map_err(|e| {
            DomainError::validation(format!("Invalid resource type '{member_code}': {e}"))
        })?;
        let mut found = self
            .registry
            .batch_get_type_schemas(
                ctx,
                &[group_id.clone(), member_id.clone()],
                ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(
                    &[EntityField::EffectiveTraits],
                ))),
            )
            .await
            .map_err(|e| registry_failure(e, group_code))?;
        let active = |schema: &Option<types_registry_sdk::TypeSchema>| {
            schema
                .as_ref()
                .is_some_and(|s| s.lifecycle_status == LifecycleStatus::Active)
        };
        if !active(found.get(&member_id).unwrap_or(&None)) {
            return Err(DomainError::validation(format!(
                "Unknown resource type: {member_code}"
            )));
        }
        match found.remove(&group_id).flatten() {
            Some(schema) if schema.lifecycle_status == LifecycleStatus::Active => {
                RgType::from_schema(schema)
            }
            _ => Err(DomainError::type_not_found(group_code)),
        }
    }
}

#[cfg(test)]
#[path = "types_registry_tests.rs"]
mod types_registry_tests;
