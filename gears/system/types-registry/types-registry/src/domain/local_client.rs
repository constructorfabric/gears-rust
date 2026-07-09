//! Local client implementing the `TypesRegistryClient` trait.
//!
//! Owns kind discrimination, recursive parent resolution, and the type-schema /
//! instance caches — service stays kind-agnostic.

// Local client is a thin adapter between the domain service (kind-agnostic
// reads/writes) and the infra-layer caches that hold `Arc<GtsTypeSchema>` /
// `Arc<GtsInstance>`. Exposing those caches via a domain-level trait would
// just be ceremony — the client owns them by construction.
#![allow(unknown_lints)]
#![allow(de0301_no_infra_in_domain)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use toolkit_canonical_errors::CanonicalError;
use toolkit_macros::domain_model;
use types_registry_sdk::{
    GtsInstance, GtsInstanceId, GtsTypeId, GtsTypeSchema, InstanceQuery, RegisterResult,
    TypeSchemaQuery, TypesRegistryClient, is_type_schema_id,
};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::model::{GtsEntity, ListQuery};
use crate::domain::service::TypesRegistryService;
use crate::infra::cache::{CacheConfig, InMemoryCache, InstanceCache, TypeSchemaCache};

/// Build the canonical `InvalidArgument` error this adapter emits for a
/// malformed / kind-mismatched GTS id, routed through the single
/// `From<DomainError> for CanonicalError` ladder (field `gts_id`, reason
/// `INVALID_GTS_ID`). The type-schema-vs-instance distinction the legacy SDK
/// error carried collapses here per ADR 0005 — the canonical boundary
/// classifies both kinds identically.
fn invalid_gts_id_err(message: impl Into<String>) -> CanonicalError {
    CanonicalError::from(DomainError::invalid_gts_id(message))
}

/// Local client for the Types Registry gear.
///
/// Implements the public [`TypesRegistryClient`] trait by wrapping
/// [`TypesRegistryService`] and adding kind discrimination, recursive
/// parent resolution, and TTL-aware caches of `Arc<GtsTypeSchema>` and
/// `Arc<GtsInstance>` so chain ancestors are deduplicated across calls.
///
/// Each cache also internally maintains a `UUID` → canonical `gts_id`
/// reverse index, populated atomically with every `put`, so `*_by_uuid`
/// lookups can short-circuit the linear storage scan once a UUID has been
/// observed. The two caches are independent: a `*_by_uuid` query only
/// consults the index of its own kind. Cross-kind UUID observations are
/// not tracked — a kind-mismatch error path costs one extra storage scan,
/// which we treat as acceptable.
#[domain_model]
pub struct TypesRegistryLocalClient {
    service: Arc<TypesRegistryService>,
    type_schemas: TypeSchemaCache,
    instances: InstanceCache,
}

impl TypesRegistryLocalClient {
    /// Creates a new local client with default cache configurations
    /// ([`CacheConfig::type_schemas`] and [`CacheConfig::instances`]).
    #[must_use]
    pub fn new(service: Arc<TypesRegistryService>) -> Self {
        Self::with_cache_configs(
            service,
            CacheConfig::type_schemas(),
            CacheConfig::instances(),
        )
    }

    /// Creates a new local client with custom cache configurations.
    #[must_use]
    pub fn with_cache_configs(
        service: Arc<TypesRegistryService>,
        type_schemas: CacheConfig,
        instances: CacheConfig,
    ) -> Self {
        Self {
            service,
            type_schemas: Box::new(InMemoryCache::new(type_schemas)),
            instances: Box::new(InMemoryCache::new(instances)),
        }
    }

    /// Drops every cached type-schema and instance, including each cache's
    /// internal UUID-index.
    ///
    /// Useful after `service.switch_to_ready()` if some entries had been built
    /// pre-ready with best-effort parents, or as a recovery hatch for tests.
    pub fn clear_caches(&self) {
        self.type_schemas.clear();
        self.instances.clear();
    }

    /// Cascade-invalidates cached entries when the type-schema with `type_id`
    /// is being rewritten. Derived type-schemas in [`Self::type_schemas`]
    /// keep `Arc<GtsTypeSchema>` parents in their chain, and instances in
    /// [`Self::instances`] embed an `Arc<GtsTypeSchema>` directly — if we
    /// drop only the rewritten key, dependents continue to return stale
    /// views. We drop every cached entry whose ancestor chain transitively
    /// references `type_id`.
    fn invalidate_type_schema_cascade(&self, type_id: &str) {
        self.type_schemas
            .retain(&|s| !s.ancestors().any(|a| a.type_id == type_id));
        self.instances
            .retain(&|i| !i.type_schema.ancestors().any(|a| a.type_id == type_id));
    }

    /// Drops the cached entry for the given type-schema id and any cached
    /// dependents (derived type-schemas, instances) that transitively
    /// reference it through their resolved chain. No-op if absent.
    pub fn invalidate_type_schema(&self, type_id: &str) {
        self.invalidate_type_schema_cascade(type_id);
    }

    /// Removes one instance entry from the cache. No-op if absent.
    pub fn invalidate_instance(&self, id: &str) {
        self.instances.invalidate(id);
    }

    /// Returns the type-schema as a fully-resolved `Arc<GtsTypeSchema>` (with
    /// all ancestors recursively populated). Caches the result.
    ///
    /// `type_id` must end with `~` — instance ids are rejected with an
    /// `InvalidArgument` canonical error (reason `INVALID_GTS_ID`) before any
    /// storage lookup, since type-schema and instance ids are lexically distinct.
    fn resolve_type_schema_arc(&self, type_id: &str) -> Result<Arc<GtsTypeSchema>, CanonicalError> {
        if !is_type_schema_id(type_id) {
            return Err(invalid_gts_id_err(format!(
                "{type_id} does not end with `~`",
            )));
        }
        if let Some(cached) = self.type_schemas.get(type_id) {
            return Ok(cached);
        }
        let entity = self.service.get(type_id).map_err(CanonicalError::from)?;
        let arc = self.build_type_schema_arc(entity)?;
        self.type_schemas
            .put(arc.type_id.to_string(), Arc::clone(&arc));
        Ok(arc)
    }

    /// Storage-backed resolution for `get_*_by_uuid` cache misses. Skips
    /// the by-uuid cache check (callers already established the miss);
    /// fetches the entity from storage by UUID, validates kind, then
    /// delegates to [`Self::get_type_schema`] so the typed cache absorbs
    /// the build (and the reverse `uuid → gts_id` index gets populated for
    /// next time). TODO(#1630): replace the linear scan in
    /// `service.get_by_uuid` with an indexed lookup.
    async fn fetch_type_schema_by_uuid_uncached(
        &self,
        type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        let entity = self
            .service
            .get_by_uuid(type_uuid)
            .map_err(CanonicalError::from)?;
        if !entity.is_type_schema {
            // The UUID exists but points to an instance — from the
            // type-schema namespace's perspective, it's not registered.
            return Err(CanonicalError::from(DomainError::not_found_by_uuid(
                type_uuid,
            )));
        }
        let gts_id = entity.gts_id.clone();
        self.get_type_schema(&gts_id).await
    }

    /// Symmetric of [`Self::fetch_type_schema_by_uuid_uncached`] for
    /// instances.
    async fn fetch_instance_by_uuid_uncached(
        &self,
        uuid: Uuid,
    ) -> Result<GtsInstance, CanonicalError> {
        let entity = self
            .service
            .get_by_uuid(uuid)
            .map_err(CanonicalError::from)?;
        if entity.is_type_schema {
            // The UUID exists but points to a type-schema — from the
            // instance namespace's perspective, it's not registered.
            return Err(CanonicalError::from(DomainError::not_found_by_uuid(uuid)));
        }
        let gts_id = entity.gts_id.clone();
        self.get_instance(&gts_id).await
    }

    /// Returns the instance as a fully-resolved `Arc<GtsInstance>`. Caches
    /// the result. Type-schema reference is resolved via the type-schema cache.
    ///
    /// `id` must NOT end with `~` — type-schema ids are rejected with an
    /// `InvalidArgument` canonical error (reason `INVALID_GTS_ID`) before any
    /// storage lookup.
    fn resolve_instance_arc(&self, id: &str) -> Result<Arc<GtsInstance>, CanonicalError> {
        if is_type_schema_id(id) {
            return Err(invalid_gts_id_err(format!(
                "{id} ends with `~` (looks like a type-schema id)",
            )));
        }
        if let Some(cached) = self.instances.get(id) {
            return Ok(cached);
        }
        let entity = self.service.get(id).map_err(CanonicalError::from)?;
        let inst = self.build_instance(entity)?;
        let arc = Arc::new(inst);
        self.instances.put(arc.id.to_string(), Arc::clone(&arc));
        Ok(arc)
    }

    /// Builds an `Arc<GtsTypeSchema>` from an internal entity, resolving the
    /// GTS chain parent (derived from the type's own `gts_id`) through the
    /// type-schema cache. Mirrors gts-rust's chain semantics in
    /// `validate_schema_chain` — the parent is the type whose id is `gts_id`
    /// minus the trailing `~`-segment. Mixin `$ref`s in `allOf` (if any) are
    /// not surfaced as parents — they're left in `schema.schema` for
    /// `effective_schema()` to observe but pass through.
    ///
    /// Does not itself insert into the cache (caller decides).
    ///
    // TODO(#1723): once gts-rust exposes a `resolve_schema(gts_id)` helper
    // returning a fully-resolved view, replace this manual chain walk with
    // a single delegation.
    fn build_type_schema_arc(
        &self,
        entity: GtsEntity,
    ) -> Result<Arc<GtsTypeSchema>, CanonicalError> {
        let parent = if let Some(parent_id) = GtsTypeSchema::derive_parent_type_id(&entity.gts_id) {
            Some(
                self.resolve_type_schema_arc(parent_id.as_ref())
                    .map_err(|e| {
                        if matches!(e, CanonicalError::NotFound { .. }) {
                            invalid_gts_id_err(format!(
                                "type-schema {} references missing parent {parent_id}",
                                entity.gts_id
                            ))
                        } else {
                            e
                        }
                    })?,
            )
        } else {
            None
        };
        let type_id = GtsTypeId::new(&entity.gts_id);
        let schema = GtsTypeSchema::try_new(type_id, entity.content, entity.description, parent)?;
        Ok(Arc::new(schema))
    }

    /// Parent existence pre-check used by `register_*` methods in ready
    /// phase. Returns `Some(error)` if the entity has a parent type-schema
    /// that is not yet registered; `None` if the entity is a root type-
    /// schema, has no extractable id, or its parent is registered.
    ///
    /// For type-schemas, the parent is the chain prefix (`derive_parent_type_id`).
    /// For instances, the parent is the declaring type-schema (`derive_type_id`).
    fn parent_pre_check(&self, gts_id: Option<&str>) -> Option<CanonicalError> {
        let id = gts_id?;
        let parent_type_id = if is_type_schema_id(id) {
            // Type-schema: parent only exists for chained (non-root) ids.
            GtsTypeSchema::derive_parent_type_id(id)?
        } else {
            // Instance: declaring type-schema is required.
            GtsInstance::derive_type_id(id)?
        };
        if self.service.exists(parent_type_id.as_ref()) {
            None
        } else {
            Some(CanonicalError::from(
                DomainError::ParentTypeSchemaNotRegistered {
                    parent_type_id: parent_type_id.into_string(),
                    dependent_id: id.to_owned(),
                },
            ))
        }
    }

    /// Builds a `GtsInstance` from an internal entity by resolving its
    /// type-schema through the cache.
    fn build_instance(&self, entity: GtsEntity) -> Result<GtsInstance, CanonicalError> {
        if entity.is_type_schema {
            return Err(invalid_gts_id_err(format!(
                "{} is a type-schema, not an instance",
                entity.gts_id,
            )));
        }
        let type_id = GtsInstance::derive_type_id(&entity.gts_id).ok_or_else(|| {
            invalid_gts_id_err(format!(
                "instance gts_id {} has no type-schema chain (no `~`)",
                entity.gts_id
            ))
        })?;
        let type_schema = self.resolve_type_schema_arc(type_id.as_ref())?;
        let segment = &entity.gts_id[type_id.as_ref().len()..];
        let instance_id = GtsInstanceId::new(type_id.as_ref(), segment);
        GtsInstance::try_new(instance_id, entity.content, entity.description, type_schema)
    }
}

/// Lexicographic comparator for optional GTS ids. `None` (no extractable
/// id from the input JSON) sorts last, so format-rejection happens at the
/// end of the batch.
fn compare_optional_gts_ids(a: Option<&str>, b: Option<&str>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(x), Some(y)) => x.cmp(y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[async_trait]
impl TypesRegistryClient for TypesRegistryLocalClient {
    async fn register(
        &self,
        entities: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        // Sort by extracted gts_id so parents register before children within
        // the same batch (items without an extractable id go to the end —
        // they'll fail in service.register with InvalidGtsId), but keep the
        // original index so the returned vec stays positionally aligned with
        // the caller's input. Caller-side correlation is the only way to
        // recover identity for items where extract_gts_id returned None.
        let mut indexed: Vec<(usize, Option<String>, serde_json::Value)> = entities
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i, self.service.extract_gts_id(&v), v))
            .collect();
        indexed.sort_by(|a, b| compare_optional_gts_ids(a.1.as_deref(), b.1.as_deref()));

        let total = indexed.len();
        let mut slots: Vec<Option<RegisterResult>> = (0..total).map(|_| None).collect();
        for (orig_idx, gts_id, value) in indexed {
            // Pre-check parent in ready phase. Skipped in config phase
            // because temporary storage holds yet-unvalidated chains.
            if self.service.is_ready()
                && let Some(err) = self.parent_pre_check(gts_id.as_deref())
            {
                slots[orig_idx] = Some(RegisterResult::Err {
                    gts_id: gts_id.clone(),
                    error: err,
                });
                continue;
            }
            let single = self.service.register(vec![value]);
            if let Some(result) = single.into_iter().next() {
                // Invalidate caches only after the write commits — evicting
                // pre-write opens a TOCTOU window where a concurrent reader
                // can repopulate the cache with the old entity from storage.
                // Use the canonical persisted id from `RegisterResult::Ok`
                // rather than the request-extracted `gts_id`: if the service
                // ever canonicalises an id (whitespace, prefix, casing),
                // invalidating by the un-canonicalised input would leave the
                // real cache entry stale. For type-schemas we also
                // cascade-invalidate dependents whose chain references this id.
                if let RegisterResult::Ok {
                    gts_id: persisted_id,
                } = &result
                {
                    if is_type_schema_id(persisted_id) {
                        self.invalidate_type_schema_cascade(persisted_id);
                    } else {
                        self.instances.invalidate(persisted_id);
                    }
                }
                slots[orig_idx] = Some(result);
            }
        }
        Ok(slots.into_iter().map(Option::unwrap).collect())
    }

    async fn register_type_schemas(
        &self,
        type_schemas: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        // See `register` for the sort-then-write-back-by-original-index pattern:
        // sort lets parents register before children in the batch, but the
        // returned vec must still line up with the caller's input order.
        let mut indexed: Vec<(usize, Option<String>, serde_json::Value)> = type_schemas
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i, self.service.extract_gts_id(&v), v))
            .collect();
        indexed.sort_by(|a, b| compare_optional_gts_ids(a.1.as_deref(), b.1.as_deref()));

        let total = indexed.len();
        let mut slots: Vec<Option<RegisterResult>> = (0..total).map(|_| None).collect();
        for (orig_idx, gts_id, value) in indexed {
            // Missing id: caller invoked the type-schema-typed endpoint, so
            // we owe a kind-typed error.
            let Some(ref id) = gts_id else {
                slots[orig_idx] = Some(RegisterResult::Err {
                    gts_id: None,
                    error: invalid_gts_id_err("no GTS id field found in entity"),
                });
                continue;
            };
            // Kind check: type-schema id must end with `~`.
            if !is_type_schema_id(id) {
                slots[orig_idx] = Some(RegisterResult::Err {
                    gts_id: gts_id.clone(),
                    error: invalid_gts_id_err(format!("{id} does not end with `~`")),
                });
                continue;
            }
            // Parent pre-check (ready phase only).
            if self.service.is_ready()
                && let Some(err) = self.parent_pre_check(Some(id))
            {
                slots[orig_idx] = Some(RegisterResult::Err {
                    gts_id: gts_id.clone(),
                    error: err,
                });
                continue;
            }
            let single = self.service.register(vec![value]);
            if let Some(result) = single.into_iter().next() {
                // Cascade-invalidate after the write commits, keyed on the
                // canonical persisted id from `RegisterResult::Ok` rather
                // than the request-extracted `id` — see the matching
                // comment in `register` for the rationale.
                if let RegisterResult::Ok {
                    gts_id: persisted_id,
                } = &result
                {
                    self.invalidate_type_schema_cascade(persisted_id);
                }
                slots[orig_idx] = Some(result);
            }
        }
        Ok(slots.into_iter().map(Option::unwrap).collect())
    }

    async fn get_type_schema(&self, type_id: &str) -> Result<GtsTypeSchema, CanonicalError> {
        let arc = self.resolve_type_schema_arc(type_id)?;
        Ok((*arc).clone())
    }

    async fn get_type_schema_by_uuid(
        &self,
        type_uuid: Uuid,
    ) -> Result<GtsTypeSchema, CanonicalError> {
        // Fast path: full cache hit by UUID. (Cache puts populate the
        // reverse uuid → gts_id index atomically, so anything previously
        // resolved on the type-schema side is reachable from here.)
        if let Some(arc) = self.type_schemas.get_by_uuid(type_uuid) {
            return Ok((*arc).clone());
        }
        self.fetch_type_schema_by_uuid_uncached(type_uuid).await
    }

    async fn get_type_schemas(
        &self,
        type_ids: Vec<String>,
    ) -> HashMap<String, Result<GtsTypeSchema, CanonicalError>> {
        let mut out = HashMap::with_capacity(type_ids.len());

        // Phase 1: format check + dedup. Format-rejected ids land directly
        // in the result map; the rest go to phase 2 for cache lookup.
        let mut to_resolve: Vec<String> = Vec::new();
        for id in type_ids {
            if out.contains_key(&id) {
                continue;
            }
            if is_type_schema_id(&id) {
                to_resolve.push(id);
            } else {
                out.insert(
                    id.clone(),
                    Err(invalid_gts_id_err(format!("{id} does not end with `~`"))),
                );
            }
        }
        if to_resolve.is_empty() {
            return out;
        }

        // Phase 2: single-lock cache lookup for the whole batch.
        let key_refs: Vec<&str> = to_resolve.iter().map(String::as_str).collect();
        let cached = self.type_schemas.get_many(&key_refs);
        let mut to_build: Vec<String> = Vec::new();
        for (id, hit) in to_resolve.into_iter().zip(cached) {
            match hit {
                Some(arc) => {
                    out.insert(id, Ok((*arc).clone()));
                }
                None => to_build.push(id),
            }
        }

        // Phase 3: storage round-trip for misses, batched put back.
        let mut to_put: Vec<(String, Arc<GtsTypeSchema>)> = Vec::new();
        for id in to_build {
            let result = match self.service.get(&id).map_err(CanonicalError::from) {
                Ok(entity) => match self.build_type_schema_arc(entity) {
                    Ok(arc) => {
                        to_put.push((arc.type_id.to_string(), Arc::clone(&arc)));
                        Ok((*arc).clone())
                    }
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            };
            out.insert(id, result);
        }
        if !to_put.is_empty() {
            self.type_schemas.put_many(to_put);
        }

        out
    }

    async fn get_type_schemas_by_uuid(
        &self,
        type_uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsTypeSchema, CanonicalError>> {
        // Phase 1: single-lock fast path — fully cached hits come back as
        // values; misses (uuid never observed, or value evicted) come back
        // as `None`.
        let cached = self.type_schemas.get_many_by_uuid(&type_uuids);

        // Phase 2: hits use the cached value; misses go straight to the
        // storage-backed slow path. TODO(#1630): batch the slow path once
        // `service.get_by_uuid` supports it.
        let mut out = HashMap::with_capacity(type_uuids.len());
        for (uuid, hit) in type_uuids.into_iter().zip(cached) {
            if out.contains_key(&uuid) {
                continue;
            }
            let result = match hit {
                Some(arc) => Ok((*arc).clone()),
                None => self.fetch_type_schema_by_uuid_uncached(uuid).await,
            };
            out.insert(uuid, result);
        }
        out
    }

    async fn list_type_schemas(
        &self,
        query: TypeSchemaQuery,
    ) -> Result<Vec<GtsTypeSchema>, CanonicalError> {
        let entities = self
            .service
            .list(&ListQuery::from_type_schema_query(query))
            .map_err(CanonicalError::from)?;
        let mut out = Vec::with_capacity(entities.len());
        for e in entities {
            if !e.is_type_schema {
                return Err(invalid_gts_id_err(format!(
                    "{} is not a type-schema",
                    e.gts_id,
                )));
            }
            // Prefer cache to share Arcs with other call sites. Cache puts
            // also populate the uuid → gts_id index automatically.
            let gts_id = e.gts_id.clone();
            let arc = if let Some(cached) = self.type_schemas.get(&gts_id) {
                cached
            } else {
                let built = self.build_type_schema_arc(e)?;
                self.type_schemas
                    .put(built.type_id.to_string(), Arc::clone(&built));
                built
            };
            out.push((*arc).clone());
        }
        Ok(out)
    }

    async fn register_instances(
        &self,
        instances: Vec<serde_json::Value>,
    ) -> Result<Vec<RegisterResult>, CanonicalError> {
        // See `register` for the sort-then-write-back-by-original-index pattern.
        let mut indexed: Vec<(usize, Option<String>, serde_json::Value)> = instances
            .into_iter()
            .enumerate()
            .map(|(i, v)| (i, self.service.extract_gts_id(&v), v))
            .collect();
        indexed.sort_by(|a, b| compare_optional_gts_ids(a.1.as_deref(), b.1.as_deref()));

        let total = indexed.len();
        let mut slots: Vec<Option<RegisterResult>> = (0..total).map(|_| None).collect();
        for (orig_idx, gts_id, value) in indexed {
            // Missing id: the caller invoked the instance-typed endpoint, so
            // we owe a kind-typed error instead of letting the kind-agnostic
            // service path infer the wrong variant.
            let Some(ref id) = gts_id else {
                slots[orig_idx] = Some(RegisterResult::Err {
                    gts_id: None,
                    error: invalid_gts_id_err("no GTS id field found in entity"),
                });
                continue;
            };
            // Kind check: instance id must NOT end with `~`.
            if is_type_schema_id(id) {
                slots[orig_idx] = Some(RegisterResult::Err {
                    gts_id: gts_id.clone(),
                    error: invalid_gts_id_err(format!(
                        "{id} ends with `~` (looks like a type-schema id)",
                    )),
                });
                continue;
            }
            // Parent (declaring type-schema) pre-check (ready phase only).
            if self.service.is_ready()
                && let Some(err) = self.parent_pre_check(Some(id))
            {
                slots[orig_idx] = Some(RegisterResult::Err {
                    gts_id: gts_id.clone(),
                    error: err,
                });
                continue;
            }
            let single = self.service.register(vec![value]);
            if let Some(result) = single.into_iter().next() {
                // Invalidate after the write commits, keyed on the canonical
                // persisted id from `RegisterResult::Ok` rather than the
                // request-extracted `id` — see the matching comment in
                // `register` for the rationale.
                if let RegisterResult::Ok {
                    gts_id: persisted_id,
                } = &result
                {
                    self.instances.invalidate(persisted_id);
                }
                slots[orig_idx] = Some(result);
            }
        }
        Ok(slots.into_iter().map(Option::unwrap).collect())
    }

    async fn get_instance(&self, id: &str) -> Result<GtsInstance, CanonicalError> {
        let arc = self.resolve_instance_arc(id)?;
        Ok((*arc).clone())
    }

    async fn get_instance_by_uuid(&self, uuid: Uuid) -> Result<GtsInstance, CanonicalError> {
        // Fast path: full cache hit by UUID.
        if let Some(arc) = self.instances.get_by_uuid(uuid) {
            return Ok((*arc).clone());
        }
        self.fetch_instance_by_uuid_uncached(uuid).await
    }

    async fn get_instances(
        &self,
        ids: Vec<String>,
    ) -> HashMap<String, Result<GtsInstance, CanonicalError>> {
        let mut out = HashMap::with_capacity(ids.len());

        // Phase 1: format check + dedup.
        let mut to_resolve: Vec<String> = Vec::new();
        for id in ids {
            if out.contains_key(&id) {
                continue;
            }
            if is_type_schema_id(&id) {
                out.insert(
                    id.clone(),
                    Err(invalid_gts_id_err(format!(
                        "{id} ends with `~` (looks like a type-schema id)",
                    ))),
                );
            } else {
                to_resolve.push(id);
            }
        }
        if to_resolve.is_empty() {
            return out;
        }

        // Phase 2: single-lock cache lookup for the whole batch.
        let key_refs: Vec<&str> = to_resolve.iter().map(String::as_str).collect();
        let cached = self.instances.get_many(&key_refs);
        let mut to_build: Vec<String> = Vec::new();
        for (id, hit) in to_resolve.into_iter().zip(cached) {
            match hit {
                Some(arc) => {
                    out.insert(id, Ok((*arc).clone()));
                }
                None => to_build.push(id),
            }
        }

        // Phase 3: storage round-trip for misses, batched put back.
        let mut to_put: Vec<(String, Arc<GtsInstance>)> = Vec::new();
        for id in to_build {
            let result = match self.service.get(&id).map_err(CanonicalError::from) {
                Ok(entity) => match self.build_instance(entity) {
                    Ok(inst) => {
                        let arc = Arc::new(inst);
                        to_put.push((arc.id.to_string(), Arc::clone(&arc)));
                        Ok((*arc).clone())
                    }
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            };
            out.insert(id, result);
        }
        if !to_put.is_empty() {
            self.instances.put_many(to_put);
        }

        out
    }

    async fn get_instances_by_uuid(
        &self,
        uuids: Vec<Uuid>,
    ) -> HashMap<Uuid, Result<GtsInstance, CanonicalError>> {
        // Phase 1: single-lock fast path. Hits come back as values; misses
        // (uuid never observed, or value evicted) come back as `None`.
        let cached = self.instances.get_many_by_uuid(&uuids);

        // Phase 2: hits use the cached value; misses go straight to the
        // storage-backed slow path. TODO(#1630): batch the slow path once
        // `service.get_by_uuid` supports it.
        let mut out = HashMap::with_capacity(uuids.len());
        for (uuid, hit) in uuids.into_iter().zip(cached) {
            if out.contains_key(&uuid) {
                continue;
            }
            let result = match hit {
                Some(arc) => Ok((*arc).clone()),
                None => self.fetch_instance_by_uuid_uncached(uuid).await,
            };
            out.insert(uuid, result);
        }
        out
    }

    async fn list_instances(
        &self,
        query: InstanceQuery,
    ) -> Result<Vec<GtsInstance>, CanonicalError> {
        let entities = self
            .service
            .list(&ListQuery::from_instance_query(query))
            .map_err(CanonicalError::from)?;
        let mut out = Vec::with_capacity(entities.len());
        for e in entities {
            // Cache puts populate the uuid → gts_id index automatically.
            let gts_id = e.gts_id.clone();
            let arc = if let Some(cached) = self.instances.get(&gts_id) {
                cached
            } else {
                let inst = self.build_instance(e)?;
                let new_arc = Arc::new(inst);
                self.instances
                    .put(new_arc.id.to_string(), Arc::clone(&new_arc));
                new_arc
            };
            out.push((*arc).clone());
        }
        Ok(out)
    }
}

#[cfg(test)]
#[path = "local_client_tests.rs"]
mod tests;
