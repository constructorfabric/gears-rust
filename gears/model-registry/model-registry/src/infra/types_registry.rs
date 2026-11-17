//! [`SchemaSource`] over the Types Registry API (DESIGN §3.5 Provider
//! Settings Schemas), bounded by `types_registry_deadline_ms`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;
use types_registry_sdk::{
    EntityField, FieldSelection, LifecycleStatus, Projection, ReadOptions, TypeSchema,
    TypesRegistryApi, TypesRegistryApiExt,
};

use crate::domain::error::{Dependency, DomainError};
use crate::domain::gts_validator::SchemaSource;

/// Operation name reported when Types Registry denies the schema read.
pub const SCHEMA_READ_OPERATION: &str = "types_registry.get_type_schema";

pub struct TypesRegistrySchemaSource {
    registry: Arc<dyn TypesRegistryApi>,
    deadline: Duration,
}

impl TypesRegistrySchemaSource {
    #[must_use]
    pub fn new(registry: Arc<dyn TypesRegistryApi>, deadline: Duration) -> Self {
        Self { registry, deadline }
    }
}

/// Types Registry outcome mapping of DESIGN §4 Error Handling: not found, a
/// malformed id, and a deleted schema are `None`, a denied read is
/// `NOT_PERMITTED`, everything else is dependency unavailable.
fn map_result(
    result: Result<TypeSchema, CanonicalError>,
    type_id: &gts::GtsTypeId,
) -> Result<Option<TypeSchema>, DomainError> {
    match result {
        Ok(schema) if schema.lifecycle_status == LifecycleStatus::Active => Ok(Some(schema)),
        Ok(_) | Err(CanonicalError::NotFound { .. } | CanonicalError::InvalidArgument { .. }) => {
            Ok(None)
        }
        Err(err @ CanonicalError::PermissionDenied { .. }) => {
            tracing::warn!(
                %type_id,
                error = %err,
                "types-registry refused the schema read: the caller lacks the Types Registry read grant"
            );
            Err(DomainError::not_permitted(SCHEMA_READ_OPERATION))
        }
        Err(other) => Err(DomainError::unavailable(Dependency::TypesRegistry, other)),
    }
}

#[async_trait]
impl SchemaSource for TypesRegistrySchemaSource {
    async fn type_schema(
        &self,
        ctx: &SecurityContext,
        type_id: &gts::GtsTypeId,
    ) -> Result<Option<TypeSchema>, DomainError> {
        let options = ReadOptions {
            projection: Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::ResolvedSchema,
            ])),
            ..ReadOptions::default()
        };
        let call = self.registry.get_type_schema(ctx, type_id, options);
        match tokio::time::timeout(self.deadline, call).await {
            Ok(result) => map_result(result, type_id),
            Err(_elapsed) => Err(DomainError::unavailable(
                Dependency::TypesRegistry,
                format!("get_type_schema did not answer within {:?}", self.deadline),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::ModelScope;
    use toolkit_gts::gts_id;

    const OPENAI: &str = gts_id!("cf.genai.model.provider.v1~cf.genai._.openai.v1~");

    fn type_id() -> gts::GtsTypeId {
        gts::GtsTypeId::new(OPENAI)
    }

    #[test]
    fn denied_read_is_not_permitted() {
        let err = ModelScope::permission_denied()
            .with_reason("NO_READ_GRANT")
            .create();
        assert!(matches!(
            map_result(Err(err), &type_id()),
            Err(DomainError::NotPermitted {
                operation: SCHEMA_READ_OPERATION,
                ..
            })
        ));
    }

    #[test]
    fn active_schema_is_returned() {
        let schema = crate::testing::provider_type_schema(OPENAI, serde_json::json!({}), false);
        assert!(matches!(map_result(Ok(schema), &type_id()), Ok(Some(_))));
    }

    #[test]
    fn not_found_and_deleted_are_none_and_failures_are_unavailable() {
        let not_found = ModelScope::not_found("type not found")
            .with_resource(OPENAI)
            .create();
        assert!(matches!(map_result(Err(not_found), &type_id()), Ok(None)));

        let mut deleted =
            crate::testing::provider_type_schema(OPENAI, serde_json::json!({}), false);
        deleted.lifecycle_status = LifecycleStatus::Deleted;
        assert!(matches!(map_result(Ok(deleted), &type_id()), Ok(None)));

        let down = CanonicalError::service_unavailable().create();
        assert!(matches!(
            map_result(Err(down), &type_id()),
            Err(DomainError::DependencyUnavailable {
                dependency: Dependency::TypesRegistry,
                ..
            })
        ));
    }
}
