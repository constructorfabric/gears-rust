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
    MembershipSchema, RgType, RgTypeCatalog, TypeUse, compile, concrete_membership_type,
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
        self.read_membership(ctx, membership_type, None, None).await
    }

    async fn get_membership_rule(
        &self,
        ctx: &SecurityContext,
        group_type: &str,
        binding: Option<&str>,
        resource_type: &str,
    ) -> Result<MembershipSchema, DomainError> {
        if let Some(binding) = binding {
            return self
                .read_membership(ctx, binding, Some(resource_type), Some(group_type))
                .await;
        }
        // A row predating the stored binding: the group type names it.
        let definition = self.get_type(ctx, group_type, TypeUse::NewLink).await?;
        self.read_membership(
            ctx,
            definition.membership_type()?,
            Some(resource_type),
            None,
        )
        .await
    }
}

impl RegistryTypeCatalog {
    /// One registry read of the membership schema, plus the member type and
    /// the group type when given. Errors follow the group type, then the
    /// membership schema's identifier, then the member type, then the
    /// membership schema itself — the order of a group-type read followed by
    /// a membership read.
    async fn read_membership(
        &self,
        ctx: &SecurityContext,
        membership_type: &str,
        resource_type: Option<&str>,
        group_type: Option<&str>,
    ) -> Result<MembershipSchema, DomainError> {
        let group_id = group_type
            .map(|code| {
                GtsTypeId::try_new(code)
                    .map_err(|e| DomainError::validation(format!("Invalid GTS type '{code}': {e}")))
            })
            .transpose()?;
        let membership_id = concrete_membership_type(membership_type).ok_or_else(|| {
            DomainError::validation(format!(
                "'{membership_type}' is not a concrete RG membership schema"
            ))
        });
        let resource_id = resource_type.map(|code| {
            GtsTypeId::try_new(code)
                .map_err(|error| DomainError::validation(format!("Invalid resource type: {error}")))
        });
        // Without a group type nothing precedes these: refuse without a read.
        let (membership_id, resource_id) = if group_id.is_none() {
            (Ok(membership_id?), resource_id.transpose()?.map(Ok))
        } else {
            (membership_id, resource_id)
        };

        // With a malformed identifier only the group type is read: its
        // answer precedes the local refusal, and nothing else is needed.
        let well_formed = membership_id.is_ok() && resource_id.as_ref().is_none_or(Result::is_ok);
        let mut ids: Vec<GtsTypeId> = Vec::new();
        let wanted = group_id.iter().chain(
            well_formed
                .then(|| {
                    membership_id
                        .as_ref()
                        .ok()
                        .into_iter()
                        .chain(resource_id.as_ref().and_then(|id| id.as_ref().ok()))
                })
                .into_iter()
                .flatten(),
        );
        for id in wanted {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
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
            .map_err(|error| registry_failure(error, group_type.unwrap_or(membership_type)))?;
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

        if let Some(id) = &group_id {
            let group = active(id)?.ok_or_else(|| DomainError::type_not_found(id.as_ref()))?;
            // The same compile check `get_type` makes for a new link.
            compile(group)?;
        }
        let membership_id = membership_id?;
        if let Some(id) = resource_id.transpose()?
            && active(&id)?.is_none()
        {
            return Err(DomainError::validation(format!(
                "Unknown resource type: {id}"
            )));
        }
        active(&membership_id)?
            .ok_or_else(|| DomainError::type_not_found(membership_type))
            .and_then(MembershipSchema::from_schema)
    }
}

#[cfg(test)]
#[path = "types_registry_tests.rs"]
mod types_registry_tests;
