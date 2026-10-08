//! Helper methods over the two contracts (SPEC §10.1). They are not part of either
//! contract's IR and need no registration: each trait is blanket-implemented, so
//! importing it adds its methods to the client.
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
//! use types_registry_sdk::{PlatformTypesRegistryApi, PlatformTypesRegistryApiExt, Projection};
//!
//! let registry = hub.get::<dyn PlatformTypesRegistryApi>()?;
//! let schema = registry
//!     .get_type_schema(&PlatformSecurityContext::outbound_marker(), type_id, Projection::Default)
//!     .await?;
//! ```
//!
//! A client that implements both contracts (the local client, the test fake) has each
//! helper name twice once both traits are imported: call it through a typed trait object
//! or with UFCS (`TypesRegistryApiExt::get_type_schema(&client, …)`). A `dyn` client
//! implements one contract, so it never meets that ambiguity.
//!
//! Identifier kind checks are local; a UUID of the other kind is `NotFound`.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use gts::GtsId;
use tokio_util::sync::CancellationToken;
use toolkit::tokio::time::{Instant, sleep_until};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{PlatformSecurityContext, SecurityContext};
use uuid::Uuid;

use crate::contract::PlatformTypesRegistryApi;
use crate::contract::TypesRegistryApi;
use crate::field;
use crate::gts::{OperationResource, TypeResource};
use crate::models::{
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, DeleteEntitiesRequest,
    DeleteItem, DeletionOperation, EntityField, EntityKey, EntityKind, EntityLookup,
    EntitySnapshot, FieldSelection, IdempotencyKey, ListEntitiesRequest, ListEntitiesResponse,
    OperationStatus, Projection, PublisherContext, RegisterEntitiesRequest, RegistrationOperation,
};

/// Batch-read key ceiling (SPEC C10); larger reads are split.
pub const MAX_BATCH_GET_KEYS: usize = 100;

/// Initial polling backoff, doubling to [`POLL_INTERVAL_MAX`].
/// Transports apply Retry-After inside `get_operation`; semantic models carry no pacing hints.
pub const POLL_INTERVAL_INITIAL: Duration = Duration::from_millis(50);

/// Longest interval between two operation polls.
pub const POLL_INTERVAL_MAX: Duration = Duration::from_secs(1);

/// Page limit preventing endless traversal and unbounded accumulation.
pub const MAX_LIST_PAGES: usize = 1_000;

/// Blanket-implemented helpers over [`PlatformTypesRegistryApi`].
///
/// Errors are `CanonicalError`; [`TypesRegistryError`](crate::TypesRegistryError) projects
/// them, including this trait's own `DeadlineExceeded` and `Cancelled`.
#[async_trait]
pub trait PlatformTypesRegistryApiExt: PlatformTypesRegistryApi {
    /// One-item delete batch; the single-key route carries no publisher.
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApi::delete_entities`].
    async fn delete_entity(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        entity: DeleteItem,
        publisher: PublisherContext,
        dry_run: bool,
    ) -> Result<DeletionOperation, CanonicalError> {
        self.delete_entities(
            ctx,
            key,
            DeleteEntitiesRequest {
                items: vec![entity],
                dry_run,
                publisher,
            },
        )
        .await
    }

    /// Submit and poll with exponential backoff under one deadline. Accepted writes continue
    /// after timeout/cancellation; timeout names `operation_id`, cancellation requires key replay.
    ///
    /// # Errors
    /// `InvalidArgument` for an unrepresentable deadline; submit/poll errors; `DeadlineExceeded`
    /// or `Cancelled`.
    async fn register_and_await(
        &self,
        ctx: &PlatformSecurityContext,
        key: IdempotencyKey,
        request: RegisterEntitiesRequest,
        deadline: Duration,
        cancel: &CancellationToken,
    ) -> Result<RegistrationOperation, CanonicalError> {
        let deadline = deadline_from_now(deadline)?;
        await_registration(self, ctx, key, request, deadline, cancel).await
    }

    /// Projected Type Schema by identifier.
    ///
    /// # Errors
    /// `InvalidArgument` for the wrong identifier kind, without transport; `NotFound` if absent.
    async fn get_type_schema(
        &self,
        ctx: &PlatformSecurityContext,
        type_id: &str,
        projection: Projection,
    ) -> Result<EntitySnapshot, CanonicalError> {
        let id = parse_kind(type_id, EntityKind::TypeSchema)?;
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsId(id),
            EntityKind::TypeSchema,
            projection,
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
        id: &str,
        projection: Projection,
    ) -> Result<EntitySnapshot, CanonicalError> {
        let id = parse_kind(id, EntityKind::Instance)?;
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsId(id),
            EntityKind::Instance,
            projection,
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
        projection: Projection,
    ) -> Result<EntitySnapshot, CanonicalError> {
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsUuid(type_uuid),
            EntityKind::TypeSchema,
            projection,
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
        projection: Projection,
    ) -> Result<EntitySnapshot, CanonicalError> {
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsUuid(uuid),
            EntityKind::Instance,
            projection,
        )
        .await
    }

    /// Type Schemas by identifier, read in batches of [`MAX_BATCH_GET_KEYS`]. Each key is
    /// answered on its own: absence is that key's `NotFound`, and an identifier of the
    /// wrong kind is that key's `InvalidArgument`, refused without transport.
    ///
    /// # Errors
    /// The error of a batch read that failed; the call then reads no further batch.
    async fn get_type_schemas(
        &self,
        ctx: &PlatformSecurityContext,
        type_ids: Vec<String>,
        projection: Projection,
    ) -> Result<HashMap<String, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
        get_many_by_id(
            &PlatformReads { api: self, ctx },
            type_ids,
            EntityKind::TypeSchema,
            projection,
        )
        .await
    }

    /// Instances by identifier, as [`Self::get_type_schemas`].
    ///
    /// # Errors
    /// As [`Self::get_type_schemas`].
    async fn get_instances(
        &self,
        ctx: &PlatformSecurityContext,
        ids: Vec<String>,
        projection: Projection,
    ) -> Result<HashMap<String, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
        get_many_by_id(
            &PlatformReads { api: self, ctx },
            ids,
            EntityKind::Instance,
            projection,
        )
        .await
    }

    /// Type Schemas by Registry Reference, each answered on its own; a reference to an
    /// Instance is that key's `NotFound`.
    ///
    /// # Errors
    /// As [`Self::get_type_schemas`].
    async fn get_type_schemas_by_uuid(
        &self,
        ctx: &PlatformSecurityContext,
        type_uuids: Vec<Uuid>,
        projection: Projection,
    ) -> Result<HashMap<Uuid, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
        get_many_by_uuid(
            &PlatformReads { api: self, ctx },
            type_uuids,
            EntityKind::TypeSchema,
            projection,
        )
        .await
    }

    /// Instances by Registry Reference, as [`Self::get_type_schemas_by_uuid`].
    ///
    /// # Errors
    /// As [`Self::get_type_schemas`].
    async fn get_instances_by_uuid(
        &self,
        ctx: &PlatformSecurityContext,
        uuids: Vec<Uuid>,
        projection: Projection,
    ) -> Result<HashMap<Uuid, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
        get_many_by_uuid(
            &PlatformReads { api: self, ctx },
            uuids,
            EntityKind::Instance,
            projection,
        )
        .await
    }

    /// Traverse Type Schemas from `query.page.cursor`, preserving filters and page size.
    /// Default projection includes content and all materializations; explicit selections are preserved.
    /// Traversal is complete but provides no point-in-time snapshot.
    ///
    /// # Errors
    /// `InvalidArgument` for an Instance filter; page or traversal-limit errors.
    async fn list_type_schemas(
        &self,
        ctx: &PlatformSecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<EntitySnapshot>, CanonicalError> {
        list_kind(
            &PlatformReads { api: self, ctx },
            query,
            EntityKind::TypeSchema,
        )
        .await
    }

    /// Traverse Instances as [`Self::list_type_schemas`], selecting content by default.
    ///
    /// # Errors
    /// `InvalidArgument` for a Type Schema filter; page or traversal-limit errors.
    async fn list_instances(
        &self,
        ctx: &PlatformSecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<EntitySnapshot>, CanonicalError> {
        list_kind(
            &PlatformReads { api: self, ctx },
            query,
            EntityKind::Instance,
        )
        .await
    }
}

impl<T: PlatformTypesRegistryApi + ?Sized> PlatformTypesRegistryApiExt for T {}

/// Blanket-implemented read helpers over [`TypesRegistryApi`]: the read helpers of
/// [`PlatformTypesRegistryApiExt`], with the same local kind narrowing, batching and
/// document selection, under a tenant's [`SecurityContext`]. Both traits run one
/// implementation; a tenant has no mutation helper.
#[async_trait]
pub trait TypesRegistryApiExt: TypesRegistryApi {
    /// As [`PlatformTypesRegistryApiExt::get_type_schema`].
    ///
    /// # Errors
    /// `InvalidArgument` for the wrong identifier kind, without transport; `NotFound` if absent.
    async fn get_type_schema(
        &self,
        ctx: &SecurityContext,
        type_id: &str,
        projection: Projection,
    ) -> Result<EntitySnapshot, CanonicalError> {
        let id = parse_kind(type_id, EntityKind::TypeSchema)?;
        let reads = TenantReads { api: self, ctx };
        get_one(
            &reads,
            EntityKey::GtsId(id),
            EntityKind::TypeSchema,
            projection,
        )
        .await
    }

    /// As [`PlatformTypesRegistryApiExt::get_instance`].
    ///
    /// # Errors
    /// As [`Self::get_type_schema`], for an Instance.
    async fn get_instance(
        &self,
        ctx: &SecurityContext,
        id: &str,
        projection: Projection,
    ) -> Result<EntitySnapshot, CanonicalError> {
        let id = parse_kind(id, EntityKind::Instance)?;
        let reads = TenantReads { api: self, ctx };
        get_one(
            &reads,
            EntityKey::GtsId(id),
            EntityKind::Instance,
            projection,
        )
        .await
    }

    /// As [`PlatformTypesRegistryApiExt::get_type_schema_by_uuid`].
    ///
    /// # Errors
    /// `NotFound` when absent or when the reference names an Instance.
    async fn get_type_schema_by_uuid(
        &self,
        ctx: &SecurityContext,
        type_uuid: Uuid,
        projection: Projection,
    ) -> Result<EntitySnapshot, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_one(
            &reads,
            EntityKey::GtsUuid(type_uuid),
            EntityKind::TypeSchema,
            projection,
        )
        .await
    }

    /// As [`PlatformTypesRegistryApiExt::get_instance_by_uuid`].
    ///
    /// # Errors
    /// `NotFound` when absent or when the reference names a Type Schema.
    async fn get_instance_by_uuid(
        &self,
        ctx: &SecurityContext,
        uuid: Uuid,
        projection: Projection,
    ) -> Result<EntitySnapshot, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_one(
            &reads,
            EntityKey::GtsUuid(uuid),
            EntityKind::Instance,
            projection,
        )
        .await
    }

    /// As [`PlatformTypesRegistryApiExt::get_type_schemas`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::get_type_schemas`].
    async fn get_type_schemas(
        &self,
        ctx: &SecurityContext,
        type_ids: Vec<String>,
        projection: Projection,
    ) -> Result<HashMap<String, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_id(&reads, type_ids, EntityKind::TypeSchema, projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::get_instances`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::get_type_schemas`].
    async fn get_instances(
        &self,
        ctx: &SecurityContext,
        ids: Vec<String>,
        projection: Projection,
    ) -> Result<HashMap<String, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_id(&reads, ids, EntityKind::Instance, projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::get_type_schemas_by_uuid`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::get_type_schemas`].
    async fn get_type_schemas_by_uuid(
        &self,
        ctx: &SecurityContext,
        type_uuids: Vec<Uuid>,
        projection: Projection,
    ) -> Result<HashMap<Uuid, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_uuid(&reads, type_uuids, EntityKind::TypeSchema, projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::get_instances_by_uuid`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::get_type_schemas`].
    async fn get_instances_by_uuid(
        &self,
        ctx: &SecurityContext,
        uuids: Vec<Uuid>,
        projection: Projection,
    ) -> Result<HashMap<Uuid, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_uuid(&reads, uuids, EntityKind::Instance, projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::list_type_schemas`].
    ///
    /// # Errors
    /// `InvalidArgument` for an Instance filter; page or traversal-limit errors.
    async fn list_type_schemas(
        &self,
        ctx: &SecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<EntitySnapshot>, CanonicalError> {
        list_kind(
            &TenantReads { api: self, ctx },
            query,
            EntityKind::TypeSchema,
        )
        .await
    }

    /// As [`PlatformTypesRegistryApiExt::list_instances`].
    ///
    /// # Errors
    /// `InvalidArgument` for a Type Schema filter; page or traversal-limit errors.
    async fn list_instances(
        &self,
        ctx: &SecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<EntitySnapshot>, CanonicalError> {
        list_kind(&TenantReads { api: self, ctx }, query, EntityKind::Instance).await
    }
}

impl<T: TypesRegistryApi + ?Sized> TypesRegistryApiExt for T {}

/// The two reads every helper composes, bound to one contract and its context, so one
/// implementation of the helpers serves both extension traits.
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

async fn get_one<R: EntityReads + ?Sized>(
    reads: &R,
    key: EntityKey,
    kind: EntityKind,
    projection: Projection,
) -> Result<EntitySnapshot, CanonicalError> {
    let mut lookups = reads
        .batch_get(BatchGetEntitiesRequest {
            items: vec![BatchGetItem::from(key.clone())],
            projection,
            fresh: false,
        })
        .await?;
    answer(&key, kind, lookups.0.remove(&key))
}

async fn get_many_by_id<R: EntityReads + ?Sized>(
    reads: &R,
    ids: Vec<String>,
    kind: EntityKind,
    projection: Projection,
) -> Result<HashMap<String, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
    let mut out = HashMap::with_capacity(ids.len());
    let mut keys = Vec::with_capacity(ids.len());
    for raw in ids {
        match parse_kind(&raw, kind) {
            Ok(id) => keys.push((raw, EntityKey::GtsId(id))),
            Err(e) => {
                out.insert(raw, Err(e));
            }
        }
    }
    for (raw, result) in get_many(reads, keys, kind, projection).await? {
        out.insert(raw, result);
    }
    Ok(out)
}

async fn get_many_by_uuid<R: EntityReads + ?Sized>(
    reads: &R,
    uuids: Vec<Uuid>,
    kind: EntityKind,
    projection: Projection,
) -> Result<HashMap<Uuid, Result<EntitySnapshot, CanonicalError>>, CanonicalError> {
    let keys = uuids
        .into_iter()
        .map(|uuid| (uuid, EntityKey::GtsUuid(uuid)))
        .collect();
    Ok(get_many(reads, keys, kind, projection)
        .await?
        .into_iter()
        .collect())
}

/// Reads `keys` in bounded batches. A failed batch fails the call: no later batch is
/// read, and no partial answer is returned beside the error.
async fn get_many<R: EntityReads + ?Sized, K>(
    reads: &R,
    keys: Vec<(K, EntityKey)>,
    kind: EntityKind,
    projection: Projection,
) -> Result<Vec<(K, Result<EntitySnapshot, CanonicalError>)>, CanonicalError> {
    let mut out = Vec::with_capacity(keys.len());
    let mut keys = keys.into_iter().peekable();
    while keys.peek().is_some() {
        let chunk: Vec<(K, EntityKey)> = keys.by_ref().take(MAX_BATCH_GET_KEYS).collect();
        let request = BatchGetEntitiesRequest {
            items: chunk
                .iter()
                .map(|(_, key)| BatchGetItem::from(key.clone()))
                .collect(),
            projection: projection.clone(),
            fresh: false,
        };
        let mut lookups = reads.batch_get(request).await?;
        // Move the lookup on its last use; earlier duplicate keys need clones.
        let mut left: HashMap<EntityKey, usize> = HashMap::with_capacity(chunk.len());
        for (_, key) in &chunk {
            *left.entry(key.clone()).or_default() += 1;
        }
        for (asked, key) in chunk {
            let last = left.get_mut(&key).is_none_or(|n| {
                *n -= 1;
                *n == 0
            });
            let lookup = if last {
                lookups.0.remove(&key)
            } else {
                lookups.0.get(&key).cloned()
            };
            out.push((asked, answer(&key, kind, lookup)));
        }
    }
    Ok(out)
}

async fn list_kind<R: EntityReads + ?Sized>(
    reads: &R,
    mut query: ListEntitiesRequest,
    kind: EntityKind,
) -> Result<Vec<EntitySnapshot>, CanonicalError> {
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
        items.extend(page.items);
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
    Err(TypeResource::invalid_argument()
        .with_field_violation(
            field::PAGE_FIELD,
            format!(
                "the listing did not end within {MAX_LIST_PAGES} pages; narrow the \
                 query or raise its page limit"
            ),
            field::INVALID_QUERY,
        )
        .create())
}

/// Default list documents: Instance content; Type Schema content and three materializations.
/// Explicit selections stay unchanged.
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

/// Submit and poll under one deadline; shared by `register_and_await` and reconciliation.
pub(crate) async fn await_registration<A: PlatformTypesRegistryApi + ?Sized>(
    api: &A,
    ctx: &PlatformSecurityContext,
    key: IdempotencyKey,
    request: RegisterEntitiesRequest,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<RegistrationOperation, CanonicalError> {
    let mut operation = bounded(
        deadline,
        cancel,
        None,
        api.register_entities(ctx, key, request),
    )
    .await??;
    let operation_id = operation.operation_id;
    let mut interval = POLL_INTERVAL_INITIAL;
    while operation.status != OperationStatus::Completed {
        bounded(
            deadline,
            cancel,
            Some(operation_id),
            sleep_until(Instant::now() + interval),
        )
        .await?;
        interval = (interval * 2).min(POLL_INTERVAL_MAX);
        let polled = bounded(
            deadline,
            cancel,
            Some(operation_id),
            api.get_operation(ctx, operation_id),
        )
        .await??;
        let crate::models::Operation::Registration(polled) = polled else {
            return Err(OperationResource::unknown(format!(
                "the registry answered a poll of registration operation {operation_id} \
                 with a deletion"
            ))
            .with_resource(operation_id.to_string())
            .create());
        };
        operation = polled;
    }
    Ok(operation)
}

/// Equal jitter in [backoff/2, backoff] spreads concurrent retries.
pub(crate) fn jittered(backoff: Duration) -> Duration {
    use rand::RngExt as _;
    let half = backoff / 2;
    let spread = u64::try_from(half.as_nanos()).unwrap_or(u64::MAX);
    half + Duration::from_nanos(rand::rng().random_range(0..=spread))
}

/// `now + budget`, or `InvalidArgument` for a budget no clock can represent.
pub(crate) fn deadline_from_now(budget: Duration) -> Result<Instant, CanonicalError> {
    Instant::now().checked_add(budget).ok_or_else(|| {
        TypeResource::invalid_argument()
            .with_field_violation(
                field::DEADLINE_FIELD,
                format!("a deadline of {budget:?} from now cannot be represented"),
                field::INVALID_DEADLINE,
            )
            .create()
    })
}

/// Deadline/cancellation win before the first poll and when simultaneously ready.
/// `timeout_at` alone polls first, allowing a ready write after expiry.
pub(crate) async fn bounded<F: std::future::Future>(
    deadline: Instant,
    cancel: &CancellationToken,
    operation_id: Option<Uuid>,
    future: F,
) -> Result<F::Output, CanonicalError> {
    if cancel.is_cancelled() {
        return Err(stopped(operation_id, StopCause::Cancelled));
    }
    if Instant::now() >= deadline {
        return Err(stopped(operation_id, StopCause::Deadline));
    }
    toolkit::tokio::select! {
        biased;
        () = cancel.cancelled() => Err(stopped(operation_id, StopCause::Cancelled)),
        () = sleep_until(deadline) => Err(stopped(operation_id, StopCause::Deadline)),
        outcome = future => Ok(outcome),
    }
}

enum StopCause {
    Cancelled,
    Deadline,
}

/// Stop waiting while accepted writes continue; timeout names the operation, cancellation
/// requires replay with the caller’s key.
fn stopped(operation_id: Option<Uuid>, cause: StopCause) -> CanonicalError {
    match (cause, operation_id) {
        (StopCause::Cancelled, _) => OperationResource::cancelled().create(),
        (StopCause::Deadline, Some(id)) => OperationResource::deadline_exceeded(format!(
            "the deadline passed while operation {id} was still running; it was not \
             cancelled, and retrying with the same idempotency key replays it"
        ))
        .with_resource(id.to_string())
        .create(),
        (StopCause::Deadline, None) => OperationResource::deadline_exceeded(
            "the deadline passed before the registry acknowledged the submission; retry \
             with the same idempotency key to learn its outcome",
        )
        .create(),
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

/// One key's lookup as a snapshot of `kind`, or why there is none.
fn answer(
    key: &EntityKey,
    kind: EntityKind,
    lookup: Option<EntityLookup>,
) -> Result<EntitySnapshot, CanonicalError> {
    match lookup {
        Some(EntityLookup::Found { snapshot, .. }) if snapshot.kind == kind => Ok(*snapshot),
        Some(EntityLookup::Found { .. } | EntityLookup::NotFound) => Err(not_found(key, kind)),
        Some(EntityLookup::Unchanged { .. }) | None => Err(CanonicalError::internal(format!(
            "the registry did not answer an unconditional read of {key}"
        ))
        .create()),
    }
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
