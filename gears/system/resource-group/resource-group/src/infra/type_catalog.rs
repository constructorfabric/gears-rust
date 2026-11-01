// Created: 2026-10-12 by Constructor Tech
//! Types Registry adapter for the [`TypeCatalog`] port.
//!
//! Reads Type Schemas through the platform-plane contract with one fixed
//! projection (effective traits + resolved schema). There is deliberately no
//! cache here: caching belongs to the Types Registry SDK.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::Deserialize;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::PlatformSecurityContext;
use types_registry_sdk::models::MAX_BATCH_GET_KEYS;
use types_registry_sdk::{
    BatchGetEntitiesRequest, BatchGetItem, EntityField, EntityKey, EntityKind, EntityLookup,
    FieldSelection, GtsId, GtsTypeId, PlatformTypesRegistryApi, Projection, TypeSchema,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::type_catalog::{ResolvedTypes, RgTypeDef, TypeCatalog};

/// Topology traits of a resource-group type (`x-gts-traits` of the RG base
/// type). Non-RG types, e.g. arbitrary membership resource types, carry none
/// of them and read as the defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RgTraits {
    can_be_root: bool,
    allowed_parent_types: Vec<String>,
    allowed_membership_types: Vec<String>,
}

/// [`TypeCatalog`] backed by the Types Registry platform API.
pub struct TrTypeCatalog {
    api: Arc<dyn PlatformTypesRegistryApi>,
}

impl TrTypeCatalog {
    #[must_use]
    pub fn new(api: Arc<dyn PlatformTypesRegistryApi>) -> Self {
        Self { api }
    }

    fn projection() -> Projection {
        Projection::Select(FieldSelection::with(&[
            EntityField::EffectiveTraits,
            EntityField::ResolvedSchema,
        ]))
    }
}

/// Any registry failure is an availability problem for this gear; the
/// original error is logged, never surfaced.
fn unavailable(e: &CanonicalError) -> DomainError {
    tracing::warn!(error = %e, "resource-group: types registry request failed");
    DomainError::types_registry_unavailable("types registry request failed")
}

fn to_def(schema: TypeSchema) -> RgTypeDef {
    let traits: RgTraits = schema
        .effective_traits
        .and_then(|doc| serde_json::from_value(doc).ok())
        .unwrap_or_default();
    RgTypeDef {
        uuid: schema.type_uuid,
        code: schema.type_id.as_ref().to_owned(),
        can_be_root: traits.can_be_root,
        allowed_parent_types: traits.allowed_parent_types,
        allowed_membership_types: traits.allowed_membership_types,
        schema: schema.resolved_schema,
    }
}

#[async_trait::async_trait]
impl TypeCatalog for TrTypeCatalog {
    async fn resolve(
        &self,
        codes: &[String],
        uuids: &[Uuid],
    ) -> Result<ResolvedTypes, DomainError> {
        // A code that is not a valid Type Schema id cannot name a registered
        // type: it is absent, not an error.
        let mut keys: Vec<EntityKey> = Vec::with_capacity(codes.len() + uuids.len());
        let mut seen: HashSet<EntityKey> = HashSet::new();
        let mut code_keys: Vec<(&String, EntityKey)> = Vec::new();
        for code in codes {
            let Some(key) = GtsTypeId::try_new(code)
                .ok()
                .and_then(|_| GtsId::try_new(code).ok())
                .map(EntityKey::GtsId)
            else {
                continue;
            };
            if seen.insert(key.clone()) {
                keys.push(key.clone());
            }
            code_keys.push((code, key));
        }
        for &uuid in uuids {
            let key = EntityKey::GtsUuid(uuid);
            if seen.insert(key.clone()) {
                keys.push(key);
            }
        }

        let mut answers: HashMap<EntityKey, TypeSchema> = HashMap::with_capacity(keys.len());
        for chunk in keys.chunks(MAX_BATCH_GET_KEYS) {
            let request = BatchGetEntitiesRequest {
                items: chunk.iter().cloned().map(BatchGetItem::from).collect(),
                projection: Self::projection(),
                fresh: false,
            };
            let mut response = self
                .api
                .batch_get_entities(&PlatformSecurityContext::outbound_marker(), request)
                .await
                .map_err(|e| unavailable(&e))?;
            for key in chunk {
                match response.0.remove(key) {
                    Some(EntityLookup::Found { entity, .. })
                        if entity.kind == EntityKind::TypeSchema =>
                    {
                        let schema = TypeSchema::try_from(*entity).map_err(|e| {
                            unavailable(
                                &CanonicalError::internal(format!(
                                    "the registry answered a type read with {} of inconsistent data",
                                    e.gts_id
                                ))
                                .create(),
                            )
                        })?;
                        answers.insert(key.clone(), schema);
                    }
                    // An entity of another kind or a missing one: unknown type.
                    Some(EntityLookup::Found { .. } | EntityLookup::NotFound) => {}
                    // `Unchanged` cannot answer a read without validators.
                    _ => {
                        return Err(unavailable(
                            &CanonicalError::internal(format!(
                                "the registry did not answer an unconditional read of {key}"
                            ))
                            .create(),
                        ));
                    }
                }
            }
        }

        let defs: HashMap<EntityKey, RgTypeDef> =
            answers.into_iter().map(|(k, s)| (k, to_def(s))).collect();
        let mut out = ResolvedTypes::default();
        for (code, key) in code_keys {
            if let Some(def) = defs.get(&key) {
                out.by_uuid.insert(def.uuid, def.clone());
                out.by_code.insert(code.clone(), def.clone());
            }
        }
        for &uuid in uuids {
            if let Some(def) = defs.get(&EntityKey::GtsUuid(uuid)) {
                out.by_uuid.insert(uuid, def.clone());
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "type_catalog_tests.rs"]
mod tests;
