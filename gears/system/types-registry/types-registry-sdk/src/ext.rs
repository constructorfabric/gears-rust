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
//! Single and batch Type Schema and Instance reads take [`ReadOptions`]; the `resolve_*`
//! helpers take none. Set `ReadOptions::fresh` to bypass
//! a client cache; the default keeps normal caching. On a client without a cache,
//! `fresh` has no effect.
//!
//! Identifier kind checks are local; a UUID of the other kind is `NotFound`.
//!
//! The `resolve_*` helpers translate registered IDs and Registry References, including
//! tombstones. Cache misses fetch the stored pair through
//! [`PlatformTypesRegistryApi::batch_resolve_identities`] or
//! [`TypesRegistryApi::batch_resolve_identities`]. For deterministic UUIDs before
//! registration, use [`GtsId::to_uuid`]. Whatever the implementation, a resolution
//! reply that is incomplete, contradicts itself or answers a key not asked fails as
//! `Internal`.

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
    EntityIdentity, EntityKey, EntityKind, EntityLookup, FieldSelection, Instance, JsonDocument,
    ListEntitiesRequest, ListEntitiesResponse, Projection, PublisherContext, ReadOptions,
    TypeSchema,
};
use crate::reconciliation::{ReconcileOptions, Reconciliation, reconcile};

pub use crate::models::MAX_BATCH_GET_KEYS;
pub use crate::reconciliation::submit::{POLL_INTERVAL_INITIAL, POLL_INTERVAL_MAX};

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
    /// do not stop remaining batches. Transient entity-read failures retry within the
    /// same pass limit and deadline. A terminal item rejection stops further passes
    /// after the current pass's batches, retaining all observed outcomes.
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

    /// The Registry Reference of a registered Type Schema, Active or deleted, as read.
    ///
    /// # Errors
    /// `NotFound` if not registered. `InvalidArgument` for an invalid or wrong-kind
    /// identifier, before any request.
    async fn resolve_type_uuid(
        &self,
        ctx: &PlatformSecurityContext,
        type_id: &GtsTypeId,
    ) -> Result<Uuid, CanonicalError> {
        let key = EntityKey::GtsId(parse_kind(type_id.as_ref(), EntityKind::TypeSchema)?);
        Ok(
            resolve_one::<_, TypeSchema>(&PlatformReads { api: self, ctx }, key)
                .await?
                .1,
        )
    }

    /// Registry References of Type Schemas, Active or deleted, resolved through
    /// [`PlatformTypesRegistryApi::batch_resolve_identities`] in batches of up to
    /// [`MAX_BATCH_GET_KEYS`] distinct identifiers; `None` when not registered.
    ///
    /// # Errors
    /// The error of a resolution that failed; the call then sends no further batch.
    /// `InvalidArgument`, before any request, for an identifier that is not a Type
    /// Schema's.
    async fn resolve_type_uuids(
        &self,
        ctx: &PlatformSecurityContext,
        type_ids: &[GtsTypeId],
    ) -> Result<HashMap<GtsTypeId, Option<Uuid>>, CanonicalError> {
        let keys = keys_by_id::<_, TypeSchema>(type_ids)?;
        let resolved =
            resolve_many::<_, _, TypeSchema>(&PlatformReads { api: self, ctx }, keys).await?;
        Ok(project(resolved, |(_, uuid)| uuid))
    }

    /// The identifier of the Type Schema a Registry Reference names, Active or deleted.
    ///
    /// # Errors
    /// `NotFound` when absent or when the reference names an Instance.
    async fn resolve_type_id(
        &self,
        ctx: &PlatformSecurityContext,
        type_uuid: Uuid,
    ) -> Result<GtsTypeId, CanonicalError> {
        let key = EntityKey::GtsUuid(type_uuid);
        Ok(
            resolve_one::<_, TypeSchema>(&PlatformReads { api: self, ctx }, key)
                .await?
                .0,
        )
    }

    /// Type Schema identifiers by Registry Reference, Active or deleted, resolved as
    /// [`Self::resolve_type_uuids`]; `None` when absent or an Instance.
    ///
    /// # Errors
    /// The error of a resolution that failed; the call then sends no further batch.
    async fn resolve_type_ids(
        &self,
        ctx: &PlatformSecurityContext,
        type_uuids: &[Uuid],
    ) -> Result<HashMap<Uuid, Option<GtsTypeId>>, CanonicalError> {
        let keys = keys_by_uuid(type_uuids);
        let resolved =
            resolve_many::<_, _, TypeSchema>(&PlatformReads { api: self, ctx }, keys).await?;
        Ok(project(resolved, |(type_id, _)| type_id))
    }

    /// The Registry Reference of a registered Instance, as [`Self::resolve_type_uuid`].
    ///
    /// # Errors
    /// As [`Self::resolve_type_uuid`], for an Instance.
    async fn resolve_instance_uuid(
        &self,
        ctx: &PlatformSecurityContext,
        id: &GtsInstanceId,
    ) -> Result<Uuid, CanonicalError> {
        let key = EntityKey::GtsId(parse_kind(id.as_ref(), EntityKind::Instance)?);
        Ok(
            resolve_one::<_, Instance>(&PlatformReads { api: self, ctx }, key)
                .await?
                .1,
        )
    }

    /// Registry References of Instances, as [`Self::resolve_type_uuids`].
    ///
    /// # Errors
    /// As [`Self::resolve_type_uuids`], for an Instance.
    async fn resolve_instance_uuids(
        &self,
        ctx: &PlatformSecurityContext,
        ids: &[GtsInstanceId],
    ) -> Result<HashMap<GtsInstanceId, Option<Uuid>>, CanonicalError> {
        let keys = keys_by_id::<_, Instance>(ids)?;
        let resolved =
            resolve_many::<_, _, Instance>(&PlatformReads { api: self, ctx }, keys).await?;
        Ok(project(resolved, |(_, uuid)| uuid))
    }

    /// The identifier of the Instance a Registry Reference names — the Instance's own,
    /// not its Type Schema's — Active or deleted.
    ///
    /// # Errors
    /// `NotFound` when absent or when the reference names a Type Schema.
    async fn resolve_instance_id(
        &self,
        ctx: &PlatformSecurityContext,
        uuid: Uuid,
    ) -> Result<GtsInstanceId, CanonicalError> {
        let key = EntityKey::GtsUuid(uuid);
        Ok(
            resolve_one::<_, Instance>(&PlatformReads { api: self, ctx }, key)
                .await?
                .0,
        )
    }

    /// Instance identifiers by Registry Reference, as [`Self::resolve_type_ids`];
    /// `None` when absent or a Type Schema.
    ///
    /// # Errors
    /// As [`Self::resolve_type_ids`].
    async fn resolve_instance_ids(
        &self,
        ctx: &PlatformSecurityContext,
        uuids: &[Uuid],
    ) -> Result<HashMap<Uuid, Option<GtsInstanceId>>, CanonicalError> {
        let keys = keys_by_uuid(uuids);
        let resolved =
            resolve_many::<_, _, Instance>(&PlatformReads { api: self, ctx }, keys).await?;
        Ok(project(resolved, |(id, _)| id))
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

    /// As [`PlatformTypesRegistryApiExt::resolve_type_uuid`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::resolve_type_uuid`].
    async fn resolve_type_uuid(
        &self,
        ctx: &SecurityContext,
        type_id: &GtsTypeId,
    ) -> Result<Uuid, CanonicalError> {
        let key = EntityKey::GtsId(parse_kind(type_id.as_ref(), EntityKind::TypeSchema)?);
        Ok(
            resolve_one::<_, TypeSchema>(&TenantReads { api: self, ctx }, key)
                .await?
                .1,
        )
    }

    /// As [`PlatformTypesRegistryApiExt::resolve_type_uuids`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::resolve_type_uuids`].
    async fn resolve_type_uuids(
        &self,
        ctx: &SecurityContext,
        type_ids: &[GtsTypeId],
    ) -> Result<HashMap<GtsTypeId, Option<Uuid>>, CanonicalError> {
        let keys = keys_by_id::<_, TypeSchema>(type_ids)?;
        let resolved =
            resolve_many::<_, _, TypeSchema>(&TenantReads { api: self, ctx }, keys).await?;
        Ok(project(resolved, |(_, uuid)| uuid))
    }

    /// As [`PlatformTypesRegistryApiExt::resolve_type_id`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::resolve_type_id`].
    async fn resolve_type_id(
        &self,
        ctx: &SecurityContext,
        type_uuid: Uuid,
    ) -> Result<GtsTypeId, CanonicalError> {
        let key = EntityKey::GtsUuid(type_uuid);
        Ok(
            resolve_one::<_, TypeSchema>(&TenantReads { api: self, ctx }, key)
                .await?
                .0,
        )
    }

    /// As [`PlatformTypesRegistryApiExt::resolve_type_ids`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::resolve_type_ids`].
    async fn resolve_type_ids(
        &self,
        ctx: &SecurityContext,
        type_uuids: &[Uuid],
    ) -> Result<HashMap<Uuid, Option<GtsTypeId>>, CanonicalError> {
        let keys = keys_by_uuid(type_uuids);
        let resolved =
            resolve_many::<_, _, TypeSchema>(&TenantReads { api: self, ctx }, keys).await?;
        Ok(project(resolved, |(type_id, _)| type_id))
    }

    /// As [`PlatformTypesRegistryApiExt::resolve_instance_uuid`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::resolve_instance_uuid`].
    async fn resolve_instance_uuid(
        &self,
        ctx: &SecurityContext,
        id: &GtsInstanceId,
    ) -> Result<Uuid, CanonicalError> {
        let key = EntityKey::GtsId(parse_kind(id.as_ref(), EntityKind::Instance)?);
        Ok(
            resolve_one::<_, Instance>(&TenantReads { api: self, ctx }, key)
                .await?
                .1,
        )
    }

    /// As [`PlatformTypesRegistryApiExt::resolve_instance_uuids`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::resolve_instance_uuids`].
    async fn resolve_instance_uuids(
        &self,
        ctx: &SecurityContext,
        ids: &[GtsInstanceId],
    ) -> Result<HashMap<GtsInstanceId, Option<Uuid>>, CanonicalError> {
        let keys = keys_by_id::<_, Instance>(ids)?;
        let resolved =
            resolve_many::<_, _, Instance>(&TenantReads { api: self, ctx }, keys).await?;
        Ok(project(resolved, |(_, uuid)| uuid))
    }

    /// As [`PlatformTypesRegistryApiExt::resolve_instance_id`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::resolve_instance_id`].
    async fn resolve_instance_id(
        &self,
        ctx: &SecurityContext,
        uuid: Uuid,
    ) -> Result<GtsInstanceId, CanonicalError> {
        let key = EntityKey::GtsUuid(uuid);
        Ok(
            resolve_one::<_, Instance>(&TenantReads { api: self, ctx }, key)
                .await?
                .0,
        )
    }

    /// As [`PlatformTypesRegistryApiExt::resolve_instance_ids`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::resolve_instance_ids`].
    async fn resolve_instance_ids(
        &self,
        ctx: &SecurityContext,
        uuids: &[Uuid],
    ) -> Result<HashMap<Uuid, Option<GtsInstanceId>>, CanonicalError> {
        let keys = keys_by_uuid(uuids);
        let resolved =
            resolve_many::<_, _, Instance>(&TenantReads { api: self, ctx }, keys).await?;
        Ok(project(resolved, |(id, _)| id))
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

    async fn batch_resolve(
        &self,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError>;
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

    async fn batch_resolve(
        &self,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        self.api.batch_resolve_identities(self.ctx, keys).await
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

    async fn batch_resolve(
        &self,
        keys: Vec<EntityKey>,
    ) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
        self.api.batch_resolve_identities(self.ctx, keys).await
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
    let keys = keys_by_id::<K, T>(ids)?;
    Ok(get_many(reads, keys, options).await?.into_iter().collect())
}

async fn get_many_by_uuid<R: EntityReads + ?Sized, T: Kinded>(
    reads: &R,
    uuids: &[Uuid],
    options: ReadOptions,
) -> Result<HashMap<Uuid, Option<T>>, CanonicalError> {
    Ok(get_many(reads, keys_by_uuid(uuids), options)
        .await?
        .into_iter()
        .collect())
}

/// Every identifier as a key of `T`'s kind, all validated before any read.
fn keys_by_id<K: AsRef<str> + Clone, T: Kinded>(
    ids: &[K],
) -> Result<Vec<(K, EntityKey)>, CanonicalError> {
    ids.iter()
        .map(|id| {
            Ok((
                id.clone(),
                EntityKey::GtsId(parse_kind(id.as_ref(), T::KIND)?),
            ))
        })
        .collect()
}

fn keys_by_uuid(uuids: &[Uuid]) -> Vec<(Uuid, EntityKey)> {
    uuids
        .iter()
        .map(|&uuid| (uuid, EntityKey::GtsUuid(uuid)))
        .collect()
}

/// The distinct entity keys of `keys` in first-asked order, and who asked for each.
fn distinct<K>(keys: Vec<(K, EntityKey)>) -> (Vec<EntityKey>, HashMap<EntityKey, Vec<K>>) {
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
    (distinct, askers)
}

/// Gives `answer` to everyone who asked for `key`.
fn fan_out<K, V: Clone>(
    out: &mut Vec<(K, V)>,
    askers: &mut HashMap<EntityKey, Vec<K>>,
    key: &EntityKey,
    answer: V,
) {
    let mut asked = askers.remove(key).unwrap_or_default();
    let last = asked.pop();
    out.extend(asked.into_iter().map(|k| (k, answer.clone())));
    out.extend(last.map(|k| (k, answer)));
}

/// Read bounded batches of distinct keys, keeping both spellings when asked.
/// A failed batch stops the call without partial results.
async fn get_many<R: EntityReads + ?Sized, K, T: Kinded>(
    reads: &R,
    keys: Vec<(K, EntityKey)>,
    options: ReadOptions,
) -> Result<Vec<(K, Option<T>)>, CanonicalError> {
    let mut out = Vec::with_capacity(keys.len());
    let (distinct, mut askers) = distinct(keys);
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
            fan_out(&mut out, &mut askers, key, answer);
        }
    }
    Ok(out)
}

/// What a `resolve_*` helper answers of one registered entity of `T`'s kind: its own
/// identifier and its Registry Reference.
type Resolved<T> = (<T as Kinded>::Id, Uuid);

/// Resolve distinct keys in bounded batches like [`get_many`].
/// Return `None` for absent or wrong-kind entities. Any batch failure, including a
/// reply [`checked_resolutions`] refuses, stops the call.
async fn resolve_many<R: EntityReads + ?Sized, K: Send, T: Kinded>(
    reads: &R,
    keys: Vec<(K, EntityKey)>,
) -> Result<Vec<(K, Option<Resolved<T>>)>, CanonicalError> {
    let mut out = Vec::with_capacity(keys.len());
    let (distinct, mut askers) = distinct(keys);
    for chunk in distinct.chunks(MAX_BATCH_GET_KEYS) {
        let mut identities =
            checked_resolutions(chunk, reads.batch_resolve(chunk.to_vec()).await?)?;
        for key in chunk {
            let identity = identities.remove(key).ok_or_else(|| unresolved(key))?;
            let answer = match identity {
                Some(identity) => resolved::<T>(&identity)?,
                None => None,
            };
            fan_out(&mut out, &mut askers, key, answer);
        }
    }
    Ok(out)
}

/// One key resolved as [`resolve_many`] does; nothing of the kind is the caller's `NotFound`.
async fn resolve_one<R: EntityReads + ?Sized, T: Kinded>(
    reads: &R,
    key: EntityKey,
) -> Result<Resolved<T>, CanonicalError> {
    let asked = [key.clone()];
    let identity = checked_resolutions(&asked, reads.batch_resolve(asked.to_vec()).await?)?
        .remove(&key)
        .ok_or_else(|| unresolved(&key))?;
    match identity {
        Some(identity) => resolved::<T>(&identity)?,
        None => None,
    }
    .ok_or_else(|| not_found(&key, T::KIND))
}

/// `identity` as an entity of `T`'s kind; `None` when it is of the other kind.
fn resolved<T: Kinded>(identity: &EntityIdentity) -> Result<Option<Resolved<T>>, CanonicalError> {
    if EntityKind::of(&identity.gts_id) != T::KIND {
        return Ok(None);
    }
    let id = T::kind_id(&identity.gts_id).ok_or_else(|| {
        CanonicalError::internal(format!(
            "the registry identified a {:?} as {}, not an identifier of its kind",
            T::KIND,
            identity.gts_id
        ))
        .create()
    })?;
    Ok(Some((id, identity.gts_uuid)))
}

/// Adapt a batch reader, such as a test double, to `batch_resolve_identities`.
/// Requires the `test-util` feature.
/// Read the mandatory fields in one bounded batch and validate each distinct result.
///
/// ```ignore
/// async fn batch_resolve_identities(&self, ctx: &PlatformSecurityContext, keys: Vec<EntityKey>)
///     -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
///     types_registry_sdk::ext::resolve_through_batch_get(keys, |request| {
///         self.batch_get_entities(ctx, request)
///     })
///     .await
/// }
/// ```
///
/// # Errors
/// The read's error; `Internal` when it leaves a key unanswered, answers it with a
/// snapshot its own data contradicts, or answers a key not asked.
#[cfg(any(test, feature = "test-util"))]
pub async fn resolve_through_batch_get<F, Fut>(
    keys: Vec<EntityKey>,
    read: F,
) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError>
where
    F: FnOnce(BatchGetEntitiesRequest) -> Fut + Send,
    Fut: std::future::Future<Output = Result<BatchGetEntitiesResponse, CanonicalError>> + Send,
{
    let request = BatchGetEntitiesRequest::new(keys.iter().cloned().map(BatchGetItem::new))
        .with_projection(FieldSelection::light());
    let answered = identities(&keys, read(request).await?)?;
    checked_resolutions(&keys, answered)
}

/// Read active or deleted identities in one unconditional mandatory-field batch.
/// Refuse missing, `Unchanged` or inconsistent replies.
#[cfg(any(test, feature = "test-util"))]
fn identities(
    keys: &[EntityKey],
    mut read: BatchGetEntitiesResponse,
) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
    let mut out = HashMap::with_capacity(keys.len());
    for key in keys {
        if out.contains_key(key) {
            continue;
        }
        let identity = match read.0.remove(key) {
            Some(EntityLookup::Found { entity, .. }) => Some(identity_of(key, *entity)?),
            Some(EntityLookup::NotFound) => None,
            Some(EntityLookup::Unchanged { .. }) | None => return Err(unanswered(key)),
        };
        out.insert(key.clone(), identity);
    }
    if let Some(extra) = read.0.keys().next() {
        return Err(resolution_fault(
            extra,
            "an answer although it was not asked",
        ));
    }
    Ok(out)
}

/// Require one result per distinct requested key and agreement across key
/// spellings. A mismatch, conflicting pair or one-sided absence for a found
/// entity fails the whole batch.
pub(crate) fn checked_resolutions(
    asked: &[EntityKey],
    mut answered: HashMap<EntityKey, Option<EntityIdentity>>,
) -> Result<HashMap<EntityKey, Option<EntityIdentity>>, CanonicalError> {
    let mut out = HashMap::with_capacity(answered.len());
    let mut by_id: HashMap<GtsId, Uuid> = HashMap::new();
    let mut by_uuid: HashMap<Uuid, GtsId> = HashMap::new();
    for key in asked {
        if out.contains_key(key) {
            continue;
        }
        let identity = answered.remove(key).ok_or_else(|| unresolved(key))?;
        if let Some(identity) = &identity {
            if !key.identifies(identity) {
                return Err(resolution_fault(key, "another entity's identity"));
            }
            let id = by_id
                .entry(identity.gts_id.clone())
                .or_insert(identity.gts_uuid);
            let uuid = by_uuid
                .entry(identity.gts_uuid)
                .or_insert_with(|| identity.gts_id.clone());
            if *id != identity.gts_uuid || *uuid != identity.gts_id {
                return Err(resolution_fault(
                    key,
                    "an identity conflicting within the batch",
                ));
            }
        }
        out.insert(key.clone(), identity);
    }
    if let Some(extra) = answered.keys().next() {
        return Err(resolution_fault(
            extra,
            "an answer although it was not asked",
        ));
    }
    // One snapshot answers the batch: an entity found by one spelling is not absent by
    // the other.
    for (key, identity) in &out {
        let Some(identity) = identity else { continue };
        let other = match key {
            EntityKey::GtsId(_) => EntityKey::GtsUuid(identity.gts_uuid),
            EntityKey::GtsUuid(_) => EntityKey::GtsId(identity.gts_id.clone()),
        };
        if matches!(out.get(&other), Some(None)) {
            return Err(resolution_fault(
                &other,
                "absence although the batch found its entity",
            ));
        }
    }
    Ok(out)
}

fn resolution_fault(key: &EntityKey, what: &str) -> CanonicalError {
    CanonicalError::internal(format!(
        "the registry answered the batch resolution of {key} with {what}"
    ))
    .create()
}

fn unresolved(key: &EntityKey) -> CanonicalError {
    CanonicalError::internal(format!(
        "the registry did not answer the batch resolution of {key}"
    ))
    .create()
}

/// Extract the identity after checking the exact key and kind-specific fields.
#[cfg(any(test, feature = "test-util"))]
fn identity_of(key: &EntityKey, entity: Entity) -> Result<EntityIdentity, CanonicalError> {
    check_snapshot(key, &entity)?;
    let identity = EntityIdentity {
        gts_id: entity.gts_id.clone(),
        gts_uuid: entity.gts_uuid,
    };
    let narrows = match entity.kind {
        EntityKind::TypeSchema => TypeSchema::try_from(entity).is_ok(),
        EntityKind::Instance => Instance::try_from(entity).is_ok(),
    };
    if !narrows {
        return Err(CanonicalError::internal(format!(
            "the registry answered {key} with {} of inconsistent data",
            identity.gts_id
        ))
        .create());
    }
    Ok(identity)
}

fn unanswered(key: &EntityKey) -> CanonicalError {
    CanonicalError::internal(format!(
        "the registry did not answer an unconditional read of {key}"
    ))
    .create()
}

/// Each answer reduced to the part a `resolve_*` helper returns.
fn project<K: Eq + Hash, T, V>(
    answers: impl IntoIterator<Item = (K, Option<T>)>,
    field: impl Fn(T) -> V,
) -> HashMap<K, Option<V>> {
    answers
        .into_iter()
        .map(|(key, answer)| (key, answer.map(&field)))
        .collect()
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
        Some(EntityLookup::Found { entity, .. }) => {
            check_snapshot(key, &entity)?;
            Ok((entity.kind == kind).then_some(*entity))
        }
        Some(EntityLookup::NotFound) => Ok(None),
        Some(EntityLookup::Unchanged { .. }) | None => Err(unanswered(key)),
    }
}

fn check_snapshot(key: &EntityKey, entity: &Entity) -> Result<(), CanonicalError> {
    if !key.names(entity) || !consistent(entity) {
        return Err(CanonicalError::internal(format!(
            "the registry answered {key} with another or an inconsistent entity ({})",
            entity.gts_id
        ))
        .create());
    }
    Ok(())
}

/// A kind-narrowed read's result type, the kind it carries and that kind's identifier.
trait Kinded: TryFrom<Entity, Error = Entity> + Clone + Send {
    const KIND: EntityKind;
    type Id: Clone + Send;

    /// `id` as this kind's own identifier; `None` when it is not one.
    fn kind_id(id: &GtsId) -> Option<Self::Id>;
}

impl Kinded for TypeSchema {
    const KIND: EntityKind = EntityKind::TypeSchema;
    type Id = GtsTypeId;

    fn kind_id(id: &GtsId) -> Option<GtsTypeId> {
        id.is_type()
            .then(|| GtsTypeId::try_new(id.as_ref()).ok())
            .flatten()
    }
}

impl Kinded for Instance {
    const KIND: EntityKind = EntityKind::Instance;
    type Id = GtsInstanceId;

    fn kind_id(id: &GtsId) -> Option<GtsInstanceId> {
        (!id.is_type())
            .then(|| GtsInstanceId::try_new(id.as_ref()).ok())
            .flatten()
    }
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

/// The kind field agrees with the identifier, and the snapshot carries only fields that
/// apply to its kind.
pub(crate) fn consistent(snapshot: &Entity) -> bool {
    snapshot.kind == EntityKind::of(&snapshot.gts_id) && snapshot.fits_its_kind()
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

#[cfg(test)]
#[path = "ext_resolve_tests.rs"]
mod ext_resolve_tests;
