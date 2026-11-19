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
    EntityField, FieldSelection, GtsTypeId, LifecycleStatus, Projection, ReadOptions, TypeSchema,
    TypesRegistryApi, TypesRegistryApiExt,
};

use crate::domain::error::DomainError;
use crate::domain::type_catalog::{
    MembershipSchema, RgType, RgTypeCatalog, TypeUse, concrete_membership_type,
};

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
        // A read the registry aborted (`Aborted`) is a conflict inside the
        // registry, not with this request: RG has not opened its transaction
        // yet, so the caller retries the whole operation, as for an outage.
        CanonicalError::ServiceUnavailable { .. }
        | CanonicalError::DeadlineExceeded { .. }
        | CanonicalError::ResourceExhausted { .. }
        | CanonicalError::Aborted { .. } => DomainError::ServiceUnavailable {
            retry_after_seconds: error.retry_after_seconds(),
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
        if schema.type_id != type_id {
            tracing::error!(requested = %type_id, returned = %schema.type_id, "types-registry answered for another type");
            return Err(DomainError::InternalError);
        }
        if usage == TypeUse::NewLink && schema.lifecycle_status != LifecycleStatus::Active {
            return Err(DomainError::type_not_found(code));
        }
        RgType::from_schema(&schema)
    }

    async fn get_membership_schema(
        &self,
        ctx: &SecurityContext,
        membership_type: &str,
    ) -> Result<MembershipSchema, DomainError> {
        self.read_membership(ctx, membership_type, None).await
    }

    async fn get_membership_rule(
        &self,
        ctx: &SecurityContext,
        group_type: &str,
        resource_type: &str,
    ) -> Result<MembershipSchema, DomainError> {
        let group = self.get_type(ctx, group_type, TypeUse::NewLink).await?;
        self.read_membership(ctx, group.membership_type()?, Some(resource_type))
            .await
    }
}

impl RegistryTypeCatalog {
    /// Read the membership schema and, for an addition, the candidate member together.
    async fn read_membership(
        &self,
        ctx: &SecurityContext,
        membership_type: &str,
        resource_type: Option<&str>,
    ) -> Result<MembershipSchema, DomainError> {
        let membership_id = concrete_membership_type(membership_type).ok_or_else(|| {
            DomainError::validation(format!(
                "'{membership_type}' is not a concrete RG membership schema"
            ))
        })?;
        let resource_id = resource_type
            .map(GtsTypeId::try_new)
            .transpose()
            .map_err(|error| DomainError::validation(format!("Invalid resource type: {error}")))?;
        let mut ids = vec![membership_id.clone()];
        if let Some(id) = &resource_id
            && *id != membership_id
        {
            ids.push(id.clone());
        }
        let schemas = self
            .registry
            .batch_get_type_schemas(
                ctx,
                &ids,
                ReadOptions::default().with_projection(Projection::Select(FieldSelection::with(
                    &[EntityField::Content, EntityField::ResolvedSchema],
                ))),
            )
            .await
            .map_err(|error| registry_failure(error, membership_type))?;
        let active = |id: &GtsTypeId| -> Result<Option<&TypeSchema>, DomainError> {
            let Some(schema) = schemas.get(id).and_then(Option::as_ref) else {
                return Ok(None);
            };
            if &schema.type_id != id {
                tracing::error!(requested = %id, returned = %schema.type_id, "types-registry answered for another type");
                return Err(DomainError::InternalError);
            }
            Ok((schema.lifecycle_status == LifecycleStatus::Active).then_some(schema))
        };

        if let Some(id) = resource_id {
            let member = active(&id)?
                .ok_or_else(|| DomainError::validation(format!("Unknown resource type: {id}")))?;
            // Abstractness belongs to the authored leaf, not its resolved base.
            let content = member
                .content
                .as_ref()
                .and_then(serde_json::Value::as_object)
                .ok_or(DomainError::InternalError)?;
            if content
                .get("x-gts-abstract")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                return Err(DomainError::validation(format!(
                    "Member type '{id}' is abstract"
                )));
            }
        }
        active(&membership_id)?
            .ok_or_else(|| DomainError::type_not_found(membership_type))
            .and_then(MembershipSchema::from_schema)
    }
}

#[cfg(test)]
#[path = "types_registry_tests.rs"]
mod types_registry_tests;
