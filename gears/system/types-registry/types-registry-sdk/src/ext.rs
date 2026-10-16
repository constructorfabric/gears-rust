//! Read and reconciliation helpers, available by importing the extension trait.
//!
//! # Which one a client uses
//!
//! Pick the contract by whose request the call serves, then import its helpers:
//!
//! | Calling for | Resolve from `ClientHub` | Import | Context |
//! |---|---|---|---|
//! | the gear itself: publishing, reading types for its own logic | `dyn PlatformTypesRegistryApi` | [`PlatformTypesRegistryApiExt`] | `PlatformSecurityContext::outbound_marker()` |
//! | a tenant's request, with the tenant's credentials; reads only | `dyn TypesRegistryApi` | [`TypesRegistryApiExt`] | the request's `SecurityContext` |
//!
//! ```ignore
//! use types_registry_sdk::{PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, ReadOptions};
//!
//! let registry = hub.get::<dyn PlatformTypesRegistryApi>()?;
//! let schema = registry
//!     .get_type_schema(&PlatformSecurityContext::outbound_marker(), &type_id, ReadOptions::default())
//!     .await?;
//! ```
//!
//! With both traits imported, dual-contract clients have ambiguous helper names.
//! Use a typed trait object or UFCS (`TypesRegistryApiExt::get_type_schema(&client, …)`).
//!
//! Single and batch helpers take [`ReadOptions`]. Set `ReadOptions::fresh` to bypass
//! a client cache; the default keeps normal caching. On a client without a cache,
//! `fresh` has no effect.
//!
//! Identifier kind checks are local; a UUID of the other kind is `NotFound`.

use std::collections::HashMap;
use std::hash::Hash;

use async_trait::async_trait;
use gts::{GtsId, GtsInstanceId, GtsTypeId};
use tokio_util::sync::CancellationToken;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use crate::contract::PlatformTypesRegistryApi;
use crate::contract::TypesRegistryApi;
use crate::field;
use crate::gts::TypeResource;
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, Entity, EntityField,
    EntityKey, EntityKind, EntityLookup, FieldSelection, Instance, JsonDocument,
    ListEntitiesRequest, ListEntitiesResponse, Projection, PublisherContext, ReadOptions,
    TypeSchema,
};
use crate::reconcile::{ReconcileOptions, Reconciliation, reconcile};

pub use crate::models::MAX_BATCH_GET_KEYS;
pub use crate::reconcile::submit::{POLL_INTERVAL_INITIAL, POLL_INTERVAL_MAX};

/// Page limit preventing endless traversal and unbounded accumulation.
pub const MAX_LIST_PAGES: usize = 1_000;

/// Blanket-implemented helpers over [`PlatformTypesRegistryApi`].
///
/// Single and batch reads take [`ReadOptions`] for projection and cache bypass.
/// Errors are `CanonicalError`; [`TypesRegistryError`](crate::TypesRegistryError) projects them.
#[async_trait]
pub trait PlatformTypesRegistryApiExt: PlatformTypesRegistryApi {
    /// Create missing entities and update differing content; never delete.
    ///
    /// `Ok` means all identifiers are admitted; [`Reconciliation::UpToDate`] skips submission.
    /// Identical duplicates collapse. Conflicting duplicates are rejected; other entities proceed.
    ///
    /// Submissions use `options.batch_size`, which must fit the registry's batch limit.
    /// Permanent canonical submission refusals stop the call, for local and remote clients alike.
    /// A `ResourceExhausted` with a quota retry hint resends the same payload and key after it,
    /// under the deadline. Identified validation or policy refusals reject that candidate;
    /// unattributed refusals stay pending.
    /// Unjudged entities keep earlier outcomes or become
    /// [`Skipped`](crate::ReconcilePendingCause::Skipped). Admission and polling failures
    /// do not stop remaining batches.
    ///
    /// Accepted writes are never rolled back, including on cancellation or drop.
    /// Use `cancel` and `options.deadline` rather than an outer timeout.
    ///
    /// # Errors
    /// `FailedPrecondition` contains the report; use
    /// [`ReconcileReport::from_canonical`](crate::ReconcileReport::from_canonical) for details.
    /// Caller cancellation and unrepresentable deadlines return `Cancelled` and `InvalidArgument`.
    async fn reconcile_entities_and_await(
        &self,
        ctx: &PlatformSecurityContext,
        publisher: &PublisherContext,
        desired: &[(GtsId, JsonDocument)],
        options: &ReconcileOptions,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation, CanonicalError> {
        reconcile(self, ctx, publisher, desired, options, cancel).await
    }

    /// Projected Type Schema by identifier.
    ///
    /// Use [`ReadOptions::default`] for normal caching, or set `fresh` for a fresh read.
    ///
    /// # Errors
    /// `NotFound` if absent. `InvalidArgument` for an invalid or wrong-kind identifier,
    /// before any request.
    async fn get_type_schema(
        &self,
        ctx: &PlatformSecurityContext,
        type_id: &GtsTypeId,
        options: ReadOptions,
    ) -> Result<TypeSchema, CanonicalError> {
        let id = parse_kind(type_id.as_ref(), EntityKind::TypeSchema)?;
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsId(id),
            options,
        )
        .await
    }

    /// One Instance by identifier, projected.
    ///
    /// # Errors
    /// As [`Self::get_type_schema`], for an Instance.
    async fn get_instance(
        &self,
        ctx: &PlatformSecurityContext,
        id: &GtsInstanceId,
        options: ReadOptions,
    ) -> Result<Instance, CanonicalError> {
        let id = parse_kind(id.as_ref(), EntityKind::Instance)?;
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsId(id),
            options,
        )
        .await
    }

    /// One Type Schema by Registry Reference, projected.
    ///
    /// # Errors
    /// `NotFound` when absent or when the reference names an Instance.
    async fn get_type_schema_by_uuid(
        &self,
        ctx: &PlatformSecurityContext,
        type_uuid: Uuid,
        options: ReadOptions,
    ) -> Result<TypeSchema, CanonicalError> {
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsUuid(type_uuid),
            options,
        )
        .await
    }

    /// One Instance by Registry Reference, projected.
    ///
    /// # Errors
    /// `NotFound` when absent or when the reference names a Type Schema.
    async fn get_instance_by_uuid(
        &self,
        ctx: &PlatformSecurityContext,
        uuid: Uuid,
        options: ReadOptions,
    ) -> Result<Instance, CanonicalError> {
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsUuid(uuid),
            options,
        )
        .await
    }

    /// Type Schemas by identifier, read in batches of [`MAX_BATCH_GET_KEYS`]. Every asked
    /// identifier is a key of the answer; `None` means no such Type Schema.
    ///
    /// # Errors
    /// The error of a batch read that failed; the call then reads no further batch.
    /// `InvalidArgument`, before any read, for an identifier that is not a Type Schema's.
    async fn batch_get_type_schemas(
        &self,
        ctx: &PlatformSecurityContext,
        type_ids: &[GtsTypeId],
        options: ReadOptions,
    ) -> Result<HashMap<GtsTypeId, Option<TypeSchema>>, CanonicalError> {
        get_many_by_id(&PlatformReads { api: self, ctx }, type_ids, options).await
    }

    /// Instances by identifier, as [`Self::batch_get_type_schemas`].
    ///
    /// # Errors
    /// As [`Self::batch_get_type_schemas`].
    async fn batch_get_instances(
        &self,
        ctx: &PlatformSecurityContext,
        ids: &[GtsInstanceId],
        options: ReadOptions,
    ) -> Result<HashMap<GtsInstanceId, Option<Instance>>, CanonicalError> {
        get_many_by_id(&PlatformReads { api: self, ctx }, ids, options).await
    }

    /// Type Schemas by Registry Reference, every asked reference a key of the answer;
    /// `None` when absent or when the reference names an Instance.
    ///
    /// # Errors
    /// As [`Self::batch_get_type_schemas`].
    async fn batch_get_type_schemas_by_uuid(
        &self,
        ctx: &PlatformSecurityContext,
        type_uuids: &[Uuid],
        options: ReadOptions,
    ) -> Result<HashMap<Uuid, Option<TypeSchema>>, CanonicalError> {
        get_many_by_uuid(&PlatformReads { api: self, ctx }, type_uuids, options).await
    }

    /// Instances by Registry Reference, as [`Self::batch_get_type_schemas_by_uuid`].
    ///
    /// # Errors
    /// As [`Self::batch_get_type_schemas`].
    async fn batch_get_instances_by_uuid(
        &self,
        ctx: &PlatformSecurityContext,
        uuids: &[Uuid],
        options: ReadOptions,
    ) -> Result<HashMap<Uuid, Option<Instance>>, CanonicalError> {
        get_many_by_uuid(&PlatformReads { api: self, ctx }, uuids, options).await
    }

    /// Traverse Type Schemas, preserving filters and page size. [`Projection::Default`]
    /// includes content and derived documents; explicit selections are kept as given.
    /// Cursors must match the query. Traversal provides no point-in-time snapshot.
    ///
    /// # Errors
    /// `InvalidArgument` for an Instance filter; a page's error; `ResourceExhausted` past
    /// [`MAX_LIST_PAGES`].
    async fn list_type_schemas(
        &self,
        ctx: &PlatformSecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<TypeSchema>, CanonicalError> {
        list_kind(&PlatformReads { api: self, ctx }, query).await
    }

    /// Traverse Instances as [`Self::list_type_schemas`]; [`Projection::Default`] selects
    /// content.
    ///
    /// # Errors
    /// `InvalidArgument` for a Type Schema filter; a page's error; `ResourceExhausted` past
    /// [`MAX_LIST_PAGES`].
    async fn list_instances(
        &self,
        ctx: &PlatformSecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<Instance>, CanonicalError> {
        list_kind(&PlatformReads { api: self, ctx }, query).await
    }
}

impl<T: PlatformTypesRegistryApi + ?Sized> PlatformTypesRegistryApiExt for T {}

/// Read helpers over [`TypesRegistryApi`], with the same batching, projection and
/// kind checks as [`PlatformTypesRegistryApiExt`], under a tenant's [`SecurityContext`].
#[async_trait]
pub trait TypesRegistryApiExt: TypesRegistryApi {
    /// As [`PlatformTypesRegistryApiExt::get_type_schema`].
    ///
    /// # Errors
    /// `InvalidArgument` for the wrong identifier kind, without transport; `NotFound` if absent.
    async fn get_type_schema(
        &self,
        ctx: &SecurityContext,
        type_id: &GtsTypeId,
        options: ReadOptions,
    ) -> Result<TypeSchema, CanonicalError> {
        let id = parse_kind(type_id.as_ref(), EntityKind::TypeSchema)?;
        let reads = TenantReads { api: self, ctx };
        get_one(&reads, EntityKey::GtsId(id), options).await
    }

    /// As [`PlatformTypesRegistryApiExt::get_instance`].
    ///
    /// # Errors
    /// As [`Self::get_type_schema`], for an Instance.
    async fn get_instance(
        &self,
        ctx: &SecurityContext,
        id: &GtsInstanceId,
        options: ReadOptions,
    ) -> Result<Instance, CanonicalError> {
        let id = parse_kind(id.as_ref(), EntityKind::Instance)?;
        let reads = TenantReads { api: self, ctx };
        get_one(&reads, EntityKey::GtsId(id), options).await
    }

    /// As [`PlatformTypesRegistryApiExt::get_type_schema_by_uuid`].
    ///
    /// # Errors
    /// `NotFound` when absent or when the reference names an Instance.
    async fn get_type_schema_by_uuid(
        &self,
        ctx: &SecurityContext,
        type_uuid: Uuid,
        options: ReadOptions,
    ) -> Result<TypeSchema, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_one(&reads, EntityKey::GtsUuid(type_uuid), options).await
    }

    /// As [`PlatformTypesRegistryApiExt::get_instance_by_uuid`].
    ///
    /// # Errors
    /// `NotFound` when absent or when the reference names a Type Schema.
    async fn get_instance_by_uuid(
        &self,
        ctx: &SecurityContext,
        uuid: Uuid,
        options: ReadOptions,
    ) -> Result<Instance, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_one(&reads, EntityKey::GtsUuid(uuid), options).await
    }

    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    async fn batch_get_type_schemas(
        &self,
        ctx: &SecurityContext,
        type_ids: &[GtsTypeId],
        options: ReadOptions,
    ) -> Result<HashMap<GtsTypeId, Option<TypeSchema>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_id(&reads, type_ids, options).await
    }

    /// As [`PlatformTypesRegistryApiExt::batch_get_instances`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    async fn batch_get_instances(
        &self,
        ctx: &SecurityContext,
        ids: &[GtsInstanceId],
        options: ReadOptions,
    ) -> Result<HashMap<GtsInstanceId, Option<Instance>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_id(&reads, ids, options).await
    }

    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas_by_uuid`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    async fn batch_get_type_schemas_by_uuid(
        &self,
        ctx: &SecurityContext,
        type_uuids: &[Uuid],
        options: ReadOptions,
    ) -> Result<HashMap<Uuid, Option<TypeSchema>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_uuid(&reads, type_uuids, options).await
    }

    /// As [`PlatformTypesRegistryApiExt::batch_get_instances_by_uuid`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    async fn batch_get_instances_by_uuid(
        &self,
        ctx: &SecurityContext,
        uuids: &[Uuid],
        options: ReadOptions,
    ) -> Result<HashMap<Uuid, Option<Instance>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_uuid(&reads, uuids, options).await
    }

    /// As [`PlatformTypesRegistryApiExt::list_type_schemas`].
    ///
    /// # Errors
    /// `InvalidArgument` for an Instance filter; a page's error; `ResourceExhausted` past
    /// [`MAX_LIST_PAGES`].
    async fn list_type_schemas(
        &self,
        ctx: &SecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<TypeSchema>, CanonicalError> {
        list_kind(&TenantReads { api: self, ctx }, query).await
    }

    /// As [`PlatformTypesRegistryApiExt::list_instances`].
    ///
    /// # Errors
    /// `InvalidArgument` for a Type Schema filter; a page's error; `ResourceExhausted` past
    /// [`MAX_LIST_PAGES`].
    async fn list_instances(
        &self,
        ctx: &SecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<Instance>, CanonicalError> {
        list_kind(&TenantReads { api: self, ctx }, query).await
    }
}

impl<T: TypesRegistryApi + ?Sized> TypesRegistryApiExt for T {}

/// Shared read interface bound to a contract and security context.
#[async_trait]
trait EntityReads: Sync {
    async fn batch_get(
        &self,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError>;

    async fn list(
        &self,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError>;
}

/// [`EntityReads`] over the platform contract.
struct PlatformReads<'a, A: ?Sized> {
    api: &'a A,
    ctx: &'a PlatformSecurityContext,
}

#[async_trait]
impl<A: PlatformTypesRegistryApi + ?Sized> EntityReads for PlatformReads<'_, A> {
    async fn batch_get(
        &self,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.api.batch_get_entities(self.ctx, request).await
    }

    async fn list(
        &self,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.api.list_entities(self.ctx, request).await
    }
}

/// [`EntityReads`] over the tenant contract.
struct TenantReads<'a, A: ?Sized> {
    api: &'a A,
    ctx: &'a SecurityContext,
}

#[async_trait]
impl<A: TypesRegistryApi + ?Sized> EntityReads for TenantReads<'_, A> {
    async fn batch_get(
        &self,
        request: BatchGetEntitiesRequest,
    ) -> Result<BatchGetEntitiesResponse, CanonicalError> {
        self.api.batch_get_entities(self.ctx, request).await
    }

    async fn list(
        &self,
        request: ListEntitiesRequest,
    ) -> Result<ListEntitiesResponse, CanonicalError> {
        self.api.list_entities(self.ctx, request).await
    }
}

async fn get_one<R: EntityReads + ?Sized, T: Kinded>(
    reads: &R,
    key: EntityKey,
    options: ReadOptions,
) -> Result<T, CanonicalError> {
    let mut lookups = reads
        .batch_get(BatchGetEntitiesRequest {
            items: vec![BatchGetItem::from(key.clone())],
            projection: options.projection,
            fresh: options.fresh,
        })
        .await?;
    narrow(answer(&key, T::KIND, lookups.0.remove(&key))?)
}

async fn get_many_by_id<R: EntityReads + ?Sized, K: AsRef<str> + Clone + Eq + Hash, T: Kinded>(
    reads: &R,
    ids: &[K],
    options: ReadOptions,
) -> Result<HashMap<K, Option<T>>, CanonicalError> {
    // Validate all identifiers before issuing any read.
    let keys = ids
        .iter()
        .map(|id| {
            Ok((
                id.clone(),
                EntityKey::GtsId(parse_kind(id.as_ref(), T::KIND)?),
            ))
        })
        .collect::<Result<Vec<_>, CanonicalError>>()?;
    Ok(get_many(reads, keys, options).await?.into_iter().collect())
}

async fn get_many_by_uuid<R: EntityReads + ?Sized, T: Kinded>(
    reads: &R,
    uuids: &[Uuid],
    options: ReadOptions,
) -> Result<HashMap<Uuid, Option<T>>, CanonicalError> {
    let keys = uuids
        .iter()
        .map(|&uuid| (uuid, EntityKey::GtsUuid(uuid)))
        .collect();
    Ok(get_many(reads, keys, options).await?.into_iter().collect())
}

/// Reads `keys` in bounded batches of distinct entity keys. Every asked key is answered,
/// including different spellings that name one entity. A failed batch fails the call: no
/// later batch is read, and no partial answer is returned beside the error.
async fn get_many<R: EntityReads + ?Sized, K, T: Kinded>(
    reads: &R,
    keys: Vec<(K, EntityKey)>,
    options: ReadOptions,
) -> Result<Vec<(K, Option<T>)>, CanonicalError> {
    let mut out = Vec::with_capacity(keys.len());
    // Who asked for each distinct key, in first-asked order.
    let mut askers: HashMap<EntityKey, Vec<K>> = HashMap::with_capacity(keys.len());
    let mut distinct = Vec::with_capacity(keys.len());
    for (asked, key) in keys {
        askers
            .entry(key)
            .or_insert_with_key(|key| {
                distinct.push(key.clone());
                Vec::new()
            })
            .push(asked);
    }
    for chunk in distinct.chunks(MAX_BATCH_GET_KEYS) {
        let request = BatchGetEntitiesRequest {
            items: chunk.iter().cloned().map(BatchGetItem::from).collect(),
            projection: options.projection.clone(),
            fresh: options.fresh,
        };
        let mut lookups = reads.batch_get(request).await?;
        for key in chunk {
            let answer = found(key, T::KIND, lookups.0.remove(key))?
                .map(narrow::<T>)
                .transpose()?;
            let mut asked = askers.remove(key).unwrap_or_default();
            let last = asked.pop();
            out.extend(asked.into_iter().map(|k| (k, answer.clone())));
            out.extend(last.map(|k| (k, answer)));
        }
    }
    Ok(out)
}

async fn list_kind<R: EntityReads + ?Sized, T: Kinded>(
    reads: &R,
    mut query: ListEntitiesRequest,
) -> Result<Vec<T>, CanonicalError> {
    let kind = T::KIND;
    if query.filter.kind.is_some_and(|asked| asked != kind) {
        return Err(TypeResource::invalid_argument()
            .with_field_violation(
                field::KIND_FIELD,
                format!("this helper lists {kind:?} entities only"),
                field::INVALID_QUERY,
            )
            .create());
    }
    query.filter.kind = Some(kind);
    if matches!(query.projection, Projection::Default) {
        query.projection = Projection::Select(list_default(kind));
    }
    let mut items = Vec::new();
    for _ in 0..MAX_LIST_PAGES {
        let page = reads.list(query.clone()).await?;
        for item in page.items {
            items.push(narrow(item)?);
        }
        match page.next {
            Some(next) if query.page.cursor.as_ref() == Some(&next) => {
                return Err(CanonicalError::internal(
                    "the registry answered a list page with the cursor it was given",
                )
                .create());
            }
            Some(next) => query.page.cursor = Some(next),
            None => return Ok(items),
        }
    }
    // The query is valid; the traversal outgrew the helper's bound.
    Err(TypeResource::resource_exhausted(format!(
        "the listing did not end within {MAX_LIST_PAGES} pages; narrow the query or raise its \
         page limit"
    ))
    .with_quota_violation(
        field::PAGE_FIELD,
        format!("at most {MAX_LIST_PAGES} pages per listing helper call"),
    )
    .create())
}

/// What [`Projection::Default`] selects on a list helper: Instance content; Type Schema
/// content and three materializations. Explicit selections, including one equal to the
/// default fields, stay unchanged.
fn list_default(kind: EntityKind) -> FieldSelection {
    match kind {
        EntityKind::Instance => FieldSelection::with(&[EntityField::Content]),
        EntityKind::TypeSchema => FieldSelection::with(&[
            EntityField::Content,
            EntityField::ResolvedSchema,
            EntityField::EffectiveTraits,
            EntityField::EffectiveTraitsSchema,
        ]),
    }
}

/// `raw` as an identifier of `kind`; the trailing `~` decides, locally.
fn parse_kind(raw: &str, kind: EntityKind) -> Result<GtsId, CanonicalError> {
    let id = GtsId::try_new(raw).map_err(|e| invalid_id(format!("'{raw}': {e}")))?;
    if EntityKind::of(&id) != kind {
        return Err(invalid_id(match kind {
            EntityKind::TypeSchema => format!("'{raw}' names an Instance, not a Type Schema"),
            EntityKind::Instance => format!("'{raw}' names a Type Schema, not an Instance"),
        }));
    }
    Ok(id)
}

fn invalid_id(message: String) -> CanonicalError {
    TypeResource::invalid_argument()
        .with_field_violation(field::GTS_ID_FIELD, message, field::INVALID_GTS_ID)
        .create()
}

/// One key's lookup as a snapshot of `kind`; `None` when absent or of the other kind.
/// A snapshot whose kind its own data contradicts is a protocol fault, never absence.
fn found(
    key: &EntityKey,
    kind: EntityKind,
    lookup: Option<EntityLookup>,
) -> Result<Option<Entity>, CanonicalError> {
    match lookup {
        Some(EntityLookup::Found { entity, .. }) if !consistent(&entity) => {
            Err(CanonicalError::internal(format!(
                "the registry answered {key} with a snapshot whose kind its data contradicts"
            ))
            .create())
        }
        Some(EntityLookup::Found { entity, .. }) if entity.kind == kind => Ok(Some(*entity)),
        Some(EntityLookup::Found { .. } | EntityLookup::NotFound) => Ok(None),
        Some(EntityLookup::Unchanged { .. }) | None => Err(CanonicalError::internal(format!(
            "the registry did not answer an unconditional read of {key}"
        ))
        .create()),
    }
}

/// A kind-narrowed read's result type and the kind it carries.
trait Kinded: TryFrom<Entity, Error = Entity> + Clone + Send {
    const KIND: EntityKind;
}

impl Kinded for TypeSchema {
    const KIND: EntityKind = EntityKind::TypeSchema;
}

impl Kinded for Instance {
    const KIND: EntityKind = EntityKind::Instance;
}

/// Narrow a snapshot to the requested kind; inconsistent data fails the call.
fn narrow<T: Kinded>(snapshot: Entity) -> Result<T, CanonicalError> {
    T::try_from(snapshot).map_err(|snapshot| {
        CanonicalError::internal(format!(
            "the registry answered a {:?} read with {} of another kind or inconsistent data",
            T::KIND,
            snapshot.gts_id
        ))
        .create()
    })
}

/// The kind field agrees with the identifier, and an Instance carries no derived form.
fn consistent(snapshot: &Entity) -> bool {
    snapshot.kind == EntityKind::of(&snapshot.gts_id)
        && (snapshot.kind == EntityKind::TypeSchema
            || (snapshot.resolved_schema.is_none()
                && snapshot.effective_traits.is_none()
                && snapshot.effective_traits_schema.is_none()))
}

/// A single read's answer: absence is the caller's `NotFound`.
fn answer(
    key: &EntityKey,
    kind: EntityKind,
    lookup: Option<EntityLookup>,
) -> Result<Entity, CanonicalError> {
    found(key, kind, lookup)?.ok_or_else(|| not_found(key, kind))
}

fn not_found(key: &EntityKey, kind: EntityKind) -> CanonicalError {
    let what = match kind {
        EntityKind::TypeSchema => "Type Schema",
        EntityKind::Instance => "Instance",
    };
    TypeResource::not_found(format!("No {what}: {key}"))
        .with_resource(key.to_string())
        .create()
}

#[cfg(test)]
#[path = "ext_tests.rs"]
mod ext_tests;

#[cfg(test)]
#[path = "ext_read_options_tests.rs"]
mod ext_read_options_tests;
