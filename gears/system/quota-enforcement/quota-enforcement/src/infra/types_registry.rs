//! `PlatformTypeRegistry`: the [`TypeRegistry`] port over the platform-plane
//! contract of `types-registry` (ADR-0012). Every read asks the registry
//! itself, never the client's cached copy, and is bounded by a time limit.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use gts::{GtsId, GtsTypeId};
use serde_json::Value;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::models::MAX_BATCH_GET_KEYS;
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetItem, EntityField, EntityKey, EntityLookup, FieldSelection,
    LifecycleStatus, PlatformTypesRegistryApi, Projection, TypeSchema, Validator,
};

use crate::domain::error::DomainError;
use crate::domain::ports::type_registry::{RegisteredType, TypeEtag, TypeRead, TypeRegistry};

/// The adapter over the platform registry client `init` resolved from the hub.
pub struct PlatformTypeRegistry {
    registry: Arc<dyn PlatformTypesRegistryApi>,
    deadline: Duration,
}

impl PlatformTypeRegistry {
    /// Bind to `registry`, bounding each read by `deadline`.
    #[must_use]
    pub fn new(registry: Arc<dyn PlatformTypesRegistryApi>, deadline: Duration) -> Self {
        Self { registry, deadline }
    }

    /// One batch read of at most [`MAX_BATCH_GET_KEYS`] keys.
    async fn read_chunk(
        &self,
        chunk: &[(GtsTypeId, Option<TypeEtag>)],
        out: &mut HashMap<GtsTypeId, TypeRead>,
    ) -> Result<(), DomainError> {
        let mut keys = Vec::with_capacity(chunk.len());
        let mut items = Vec::with_capacity(chunk.len());
        for (id, etag) in chunk {
            let Ok(gts_id) = GtsId::try_new(id.as_ref()) else {
                out.insert(id.clone(), TypeRead::Absent);
                continue;
            };
            let key = EntityKey::GtsId(gts_id);
            let mut item = BatchGetItem::new(key.clone());
            if let Some(etag) = etag {
                item = item.with_if_none_match(Validator::from_bytes(etag.0.clone()));
            }
            keys.push((id.clone(), key));
            items.push(item);
        }
        if items.is_empty() {
            return Ok(());
        }
        let request = BatchGetEntitiesRequest::new(items)
            .with_projection(Projection::Select(FieldSelection::with(&[
                EntityField::Content,
                EntityField::ResolvedSchema,
                EntityField::EffectiveTraits,
            ])))
            .with_fresh(true);
        let ctx = PlatformSecurityContext::outbound_marker();
        let read = self.registry.batch_get_entities(&ctx, request);
        let mut answers = match tokio::time::timeout(self.deadline, read).await {
            Ok(Ok(answers)) => answers.0,
            Ok(Err(error)) => {
                return Err(DomainError::TypesRegistryUnavailable(format!(
                    "reading {} type(s) failed: {error}",
                    keys.len()
                )));
            }
            Err(_) => {
                return Err(DomainError::TypesRegistryUnavailable(format!(
                    "reading {} type(s) reached its time limit of {:?}",
                    keys.len(),
                    self.deadline
                )));
            }
        };
        for (id, key) in keys {
            let read = match answers.remove(&key) {
                Some(EntityLookup::Found { entity, etag }) => match TypeSchema::try_from(*entity) {
                    Ok(schema) if schema.lifecycle_status != LifecycleStatus::Deleted => {
                        TypeRead::Found(Box::new(registered(schema, &etag)))
                    }
                    _ => TypeRead::Absent,
                },
                Some(EntityLookup::Unchanged { .. }) => TypeRead::Unchanged,
                Some(EntityLookup::NotFound) => TypeRead::Absent,
                Some(other) => {
                    return Err(DomainError::TypesRegistryUnavailable(format!(
                        "the registry answered {id} with {other:?}"
                    )));
                }
                None => {
                    return Err(DomainError::TypesRegistryUnavailable(format!(
                        "the registry did not answer {id}"
                    )));
                }
            };
            out.insert(id, read);
        }
        Ok(())
    }
}

/// The port's view of a type the registry returned.
fn registered(schema: TypeSchema, etag: &Validator) -> RegisteredType {
    let payload = schema
        .compile_payload_validator()
        .map_err(|error| error.to_string());
    let is_abstract = schema
        .content
        .as_ref()
        .and_then(|content| content.get("x-gts-abstract"))
        == Some(&Value::Bool(true));
    RegisteredType {
        id: schema.type_id,
        is_abstract,
        effective_traits: schema.effective_traits.unwrap_or(Value::Null),
        schema: schema.resolved_schema.unwrap_or(Value::Null),
        payload,
        etag: TypeEtag(etag.as_bytes().to_vec()),
    }
}

#[async_trait]
impl TypeRegistry for PlatformTypeRegistry {
    async fn read_types(
        &self,
        reads: Vec<(GtsTypeId, Option<TypeEtag>)>,
    ) -> Result<HashMap<GtsTypeId, TypeRead>, DomainError> {
        let mut out = HashMap::with_capacity(reads.len());
        for chunk in reads.chunks(MAX_BATCH_GET_KEYS) {
            self.read_chunk(chunk, &mut out).await?;
        }
        Ok(out)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "types_registry_tests.rs"]
mod types_registry_tests;
