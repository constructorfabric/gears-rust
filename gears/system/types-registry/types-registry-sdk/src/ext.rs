//! Helpers over both contracts (SPEC §10.1), outside contract IR. Blanket
//! implementations make the methods available by importing the extension trait.
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
//!     .get_type_schema(&PlatformSecurityContext::outbound_marker(), &type_id, Projection::Default)
//!     .await?;
//! ```
//!
//! With both traits imported, dual-contract clients have ambiguous helper names.
//! Use a typed trait object or UFCS (`TypesRegistryApiExt::get_type_schema(&client, …)`).
//!
//! Identifier kind checks are local; a UUID of the other kind is `NotFound`.

use std::collections::HashMap;
use std::hash::Hash;
use std::time::Duration;

use async_trait::async_trait;
use gts::{GtsId, GtsInstanceId, GtsTypeId};
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
    BatchGetEntitiesRequest, BatchGetEntitiesResponse, BatchGetItem, Entity, EntityField,
    EntityKey, EntityKind, EntityLookup, FieldSelection, IdempotencyKey, Instance, JsonDocument,
    ListEntitiesRequest, ListEntitiesResponse, OperationStatus, Projection, PublisherContext,
    RegisterEntitiesRequest, RegistrationOperation, TypeSchema,
};
use crate::reconcile::{ReconcileOptions, Reconciliation, reconcile};

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
    /// Reconcile `desired` under `options` (SPEC §10.1): create absent identifiers and
    /// update differing content. Safe on every start; omitted identifiers are never deleted.
    ///
    /// [`Reconciliation::UpToDate`] means all documents matched without submission.
    /// `Ok(Reconciled(..))` may include `Rejected` (e.g. incompatible changes) or `Pending`
    /// outcomes, including those left by deadline or pass exhaustion.
    ///
    /// Cancellation or dropping loses in-flight keys; accepted writes continue. A later
    /// call recovers through reads and preconditions. Prefer the token and `options.deadline`
    /// over an outer timeout.
    ///
    /// # Errors
    /// `InvalidArgument` for an unrepresentable deadline, `Cancelled` on cancellation. Registry failures are per-identifier
    /// [`ReconcileOutcome`](crate::ReconcileOutcome)s.
    async fn reconcile_entities_and_await(
        &self,
        ctx: &PlatformSecurityContext,
        publisher: &PublisherContext,
        desired: &[(String, JsonDocument)],
        options: &ReconcileOptions,
        cancel: &CancellationToken,
    ) -> Result<Reconciliation, CanonicalError> {
        reconcile(self, ctx, publisher, desired, options, cancel).await
    }

    /// Projected Type Schema by identifier.
    ///
    /// # Errors
    /// `NotFound` if absent. `InvalidArgument`, without transport, for an identifier that is
    /// not a Type Schema's, which only an unchecked `GtsTypeId::new` can build.
    async fn get_type_schema(
        &self,
        ctx: &PlatformSecurityContext,
        type_id: &GtsTypeId,
        projection: Projection,
    ) -> Result<TypeSchema, CanonicalError> {
        let id = parse_kind(type_id.as_ref(), EntityKind::TypeSchema)?;
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsId(id),
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
        id: &GtsInstanceId,
        projection: Projection,
    ) -> Result<Instance, CanonicalError> {
        let id = parse_kind(id.as_ref(), EntityKind::Instance)?;
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsId(id),
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
    ) -> Result<TypeSchema, CanonicalError> {
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsUuid(type_uuid),
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
    ) -> Result<Instance, CanonicalError> {
        get_one(
            &PlatformReads { api: self, ctx },
            EntityKey::GtsUuid(uuid),
            projection,
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
        projection: Projection,
    ) -> Result<HashMap<GtsTypeId, Option<TypeSchema>>, CanonicalError> {
        get_many_by_id(&PlatformReads { api: self, ctx }, type_ids, projection).await
    }

    /// Instances by identifier, as [`Self::batch_get_type_schemas`].
    ///
    /// # Errors
    /// As [`Self::batch_get_type_schemas`].
    async fn batch_get_instances(
        &self,
        ctx: &PlatformSecurityContext,
        ids: &[GtsInstanceId],
        projection: Projection,
    ) -> Result<HashMap<GtsInstanceId, Option<Instance>>, CanonicalError> {
        get_many_by_id(&PlatformReads { api: self, ctx }, ids, projection).await
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
        projection: Projection,
    ) -> Result<HashMap<Uuid, Option<TypeSchema>>, CanonicalError> {
        get_many_by_uuid(&PlatformReads { api: self, ctx }, type_uuids, projection).await
    }

    /// Instances by Registry Reference, as [`Self::batch_get_type_schemas_by_uuid`].
    ///
    /// # Errors
    /// As [`Self::batch_get_type_schemas`].
    async fn batch_get_instances_by_uuid(
        &self,
        ctx: &PlatformSecurityContext,
        uuids: &[Uuid],
        projection: Projection,
    ) -> Result<HashMap<Uuid, Option<Instance>>, CanonicalError> {
        get_many_by_uuid(&PlatformReads { api: self, ctx }, uuids, projection).await
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
    ) -> Result<Vec<TypeSchema>, CanonicalError> {
        list_kind(&PlatformReads { api: self, ctx }, query).await
    }

    /// Traverse Instances as [`Self::list_type_schemas`], selecting content by default.
    ///
    /// # Errors
    /// `InvalidArgument` for a Type Schema filter; page or traversal-limit errors.
    async fn list_instances(
        &self,
        ctx: &PlatformSecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<Instance>, CanonicalError> {
        list_kind(&PlatformReads { api: self, ctx }, query).await
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
        type_id: &GtsTypeId,
        projection: Projection,
    ) -> Result<TypeSchema, CanonicalError> {
        let id = parse_kind(type_id.as_ref(), EntityKind::TypeSchema)?;
        let reads = TenantReads { api: self, ctx };
        get_one(&reads, EntityKey::GtsId(id), projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::get_instance`].
    ///
    /// # Errors
    /// As [`Self::get_type_schema`], for an Instance.
    async fn get_instance(
        &self,
        ctx: &SecurityContext,
        id: &GtsInstanceId,
        projection: Projection,
    ) -> Result<Instance, CanonicalError> {
        let id = parse_kind(id.as_ref(), EntityKind::Instance)?;
        let reads = TenantReads { api: self, ctx };
        get_one(&reads, EntityKey::GtsId(id), projection).await
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
    ) -> Result<TypeSchema, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_one(&reads, EntityKey::GtsUuid(type_uuid), projection).await
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
    ) -> Result<Instance, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_one(&reads, EntityKey::GtsUuid(uuid), projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    async fn batch_get_type_schemas(
        &self,
        ctx: &SecurityContext,
        type_ids: &[GtsTypeId],
        projection: Projection,
    ) -> Result<HashMap<GtsTypeId, Option<TypeSchema>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_id(&reads, type_ids, projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::batch_get_instances`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    async fn batch_get_instances(
        &self,
        ctx: &SecurityContext,
        ids: &[GtsInstanceId],
        projection: Projection,
    ) -> Result<HashMap<GtsInstanceId, Option<Instance>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_id(&reads, ids, projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas_by_uuid`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    async fn batch_get_type_schemas_by_uuid(
        &self,
        ctx: &SecurityContext,
        type_uuids: &[Uuid],
        projection: Projection,
    ) -> Result<HashMap<Uuid, Option<TypeSchema>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_uuid(&reads, type_uuids, projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::batch_get_instances_by_uuid`].
    ///
    /// # Errors
    /// As [`PlatformTypesRegistryApiExt::batch_get_type_schemas`].
    async fn batch_get_instances_by_uuid(
        &self,
        ctx: &SecurityContext,
        uuids: &[Uuid],
        projection: Projection,
    ) -> Result<HashMap<Uuid, Option<Instance>>, CanonicalError> {
        let reads = TenantReads { api: self, ctx };
        get_many_by_uuid(&reads, uuids, projection).await
    }

    /// As [`PlatformTypesRegistryApiExt::list_type_schemas`].
    ///
    /// # Errors
    /// `InvalidArgument` for an Instance filter; page or traversal-limit errors.
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
    /// `InvalidArgument` for a Type Schema filter; page or traversal-limit errors.
    async fn list_instances(
        &self,
        ctx: &SecurityContext,
        query: ListEntitiesRequest,
    ) -> Result<Vec<Instance>, CanonicalError> {
        list_kind(&TenantReads { api: self, ctx }, query).await
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

async fn get_one<R: EntityReads + ?Sized, T: Kinded>(
    reads: &R,
    key: EntityKey,
    projection: Projection,
) -> Result<T, CanonicalError> {
    let mut lookups = reads
        .batch_get(BatchGetEntitiesRequest {
            items: vec![BatchGetItem::from(key.clone())],
            projection,
            fresh: false,
        })
        .await?;
    narrow(answer(&key, T::KIND, lookups.0.remove(&key))?)
}

async fn get_many_by_id<R: EntityReads + ?Sized, K: AsRef<str> + Clone + Eq + Hash, T: Kinded>(
    reads: &R,
    ids: &[K],
    projection: Projection,
) -> Result<HashMap<K, Option<T>>, CanonicalError> {
    // Checked before any read: a typed id built unchecked is the caller's bug, and the
    // whole call is refused rather than answered around it.
    let keys = ids
        .iter()
        .map(|id| {
            Ok((
                id.clone(),
                EntityKey::GtsId(parse_kind(id.as_ref(), T::KIND)?),
            ))
        })
        .collect::<Result<Vec<_>, CanonicalError>>()?;
    Ok(get_many(reads, keys, projection)
        .await?
        .into_iter()
        .collect())
}

async fn get_many_by_uuid<R: EntityReads + ?Sized, T: Kinded>(
    reads: &R,
    uuids: &[Uuid],
    projection: Projection,
) -> Result<HashMap<Uuid, Option<T>>, CanonicalError> {
    let keys = uuids
        .iter()
        .map(|&uuid| (uuid, EntityKey::GtsUuid(uuid)))
        .collect();
    Ok(get_many(reads, keys, projection)
        .await?
        .into_iter()
        .collect())
}

/// Reads `keys` in bounded batches. A failed batch fails the call: no later batch is
/// read, and no partial answer is returned beside the error.
async fn get_many<R: EntityReads + ?Sized, K, T: Kinded>(
    reads: &R,
    keys: Vec<(K, EntityKey)>,
    projection: Projection,
) -> Result<Vec<(K, Option<T>)>, CanonicalError> {
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
            out.push((
                asked,
                found(&key, T::KIND, lookup)?.map(narrow).transpose()?,
            ));
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

/// Submit and poll under one deadline; reconciliation's submit step.
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
trait Kinded: TryFrom<Entity, Error = Entity> + Send {
    const KIND: EntityKind;
}

impl Kinded for TypeSchema {
    const KIND: EntityKind = EntityKind::TypeSchema;
}

impl Kinded for Instance {
    const KIND: EntityKind = EntityKind::Instance;
}

/// A snapshot the registry answered for a kind-narrowed read, as that kind. One that does
/// not convert — the other kind, or inconsistent kind data — is a protocol fault, and fails
/// the whole call.
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
