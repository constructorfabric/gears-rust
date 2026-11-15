//! `IngestService` (`DESIGN.md` "`IngestService`"): producer-facing, owns the write
//! path.

use crate::domain::model::Sequence;
use async_trait::async_trait;
use authz_resolver_sdk::{AccessRequest, BarrierMode, EnforcerError, PolicyEnforcer, TenantMode};
use gts::{GtsId, GtsTypeId};
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;
use toolkit_security::{SecurityContext, pep_properties};
use uuid::Uuid;

use crate::domain::audit;
use crate::domain::authz::{
    MetadataAccess, TENANT_SCOPE_RESOURCE, event_type_resource, tenant_authorized,
    with_forbidden_code,
};
use crate::domain::backend::BackendResolver;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::idempotency::{ChainRun, ProducerIdempotencyOutcome};
use crate::domain::producer_reset::ProducerResetScope;
use event_broker_sdk::EventV1;
use event_broker_sdk::models::EventType;

use crate::domain::model::{Event, IngestStamp, Meta};
use crate::domain::streaming::scope::EVENT_SUBJECT_TYPE;

/// One event as a producer submits it: what the publish schema admits, and
/// nothing the broker derives.
///
/// A publish body names neither a topic nor a partition key. The stream an
/// event belongs to is the `topic` trait on its event type, and which member
/// determines its partition is that type's `partition_key` pointer, so an event
/// can never disagree with its type about either and there is nothing for the
/// broker to cross-check. [`IngestService::publish_event`] resolves both and
/// returns the [`Event`] the request became, with the topic stamped on it; the
/// partition is the backend's to assign.
#[domain_model]
#[derive(Debug, Clone)]
pub struct PublishRequest {
    pub id: Uuid,
    pub r#type: GtsTypeId,
    pub subject_tenant: Uuid,
    pub source: String,
    pub subject: String,
    pub subject_type: GtsTypeId,
    /// The principal that owns the event; `None` for a tenant-wide event.
    pub subject_owner: Option<Uuid>,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub trace_parent: Option<String>,
    pub data: serde_json::Value,
    pub meta: Option<PublishMeta>,
}

/// The `meta` block of a [`PublishRequest`] as the producer sent it. A field the
/// producer left out stays `None`: ADR-0004 decides a publish's mode shape from
/// which fields are present, so none is defaulted.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct PublishMeta {
    pub version: i32,
    pub producer: Option<Uuid>,
    pub previous: Option<i64>,
    pub sequence: Option<i64>,
}

impl PublishMeta {
    /// The newest `meta.version` the broker reads (ADR-0003, "Optional Versioned
    /// `meta` Block"); a newer one is refused with `UnknownMetaVersion`.
    pub const SUPPORTED_VERSION: i32 = 1;

    /// The producer the block names. A literal nil id names none, as an absent
    /// one does.
    #[must_use]
    pub fn producer(&self) -> Option<Uuid> {
        self.producer.filter(|id| !id.is_nil())
    }

    /// Refuses a block that names no producer but carries a chain field
    /// (ADR-0004, "Mode-Shape Enforcement at Publish Time"). A block with neither
    /// is a stateless publish's once [`Self::check_version`] passes it, and the
    /// caller then treats it as absent.
    fn check_without_producer(&self) -> Result<(), DomainError> {
        if self.previous.is_some() || self.sequence.is_some() {
            return Err(DomainError::Validation {
                code: ErrorCode::MetaWithoutProducerId,
                message: "meta carries previous or sequence but no producer".to_owned(),
            });
        }
        Ok(())
    }

    /// Refuses a block newer than [`Self::SUPPORTED_VERSION`] (ADR-0003,
    /// "Optional Versioned `meta` Block"), whether or not it names a producer.
    fn check_version(&self) -> Result<(), DomainError> {
        if self.version > Self::SUPPORTED_VERSION {
            return Err(DomainError::Validation {
                code: ErrorCode::UnknownMetaVersion,
                message: format!(
                    "meta.version is newer than the broker supports ({})",
                    Self::SUPPORTED_VERSION
                ),
            });
        }
        Ok(())
    }

    /// The stored `meta` of a publish whose block names `producer_id`, registered
    /// in `mode`. Checks the mode shape (ADR-0004, "Mode-Shape Enforcement at
    /// Publish Time"), then the version (ADR-0003), so a stored `Meta` always has
    /// a producer and a sequence.
    fn into_stored(self, producer_id: Uuid, mode: ProducerMode) -> Result<Meta, DomainError> {
        // The caller resolved `producer_id` from this block, so the two agree.
        debug_assert_eq!(self.producer(), Some(producer_id));
        let sequence = match (mode, self.previous, self.sequence) {
            (ProducerMode::Chained, Some(_), Some(sequence))
            | (ProducerMode::Monotonic, None, Some(sequence)) => sequence,
            (ProducerMode::Chained, ..) => {
                return Err(DomainError::Validation {
                    code: ErrorCode::ChainModeFieldsMissing,
                    message: "a chained-mode publish needs meta.previous and meta.sequence"
                        .to_owned(),
                });
            }
            (ProducerMode::Monotonic, ..) => {
                return Err(DomainError::Validation {
                    code: ErrorCode::MonotonicModeFieldsViolation,
                    message: "a monotonic-mode publish needs meta.sequence and must omit \
                              meta.previous"
                        .to_owned(),
                });
            }
        };
        self.check_version()?;
        Ok(Meta {
            version: self.version,
            producer_id,
            previous: self.previous,
            sequence,
        })
    }
}

impl PublishRequest {
    /// Rejects a `trace_parent` that is not a W3C traceparent, as the field violation
    /// the SDK recovers as `InvalidEventField`.
    pub(crate) fn check_trace_parent(trace_parent: &str) -> Result<(), DomainError> {
        if event_broker_sdk::validate::is_trace_parent(trace_parent) {
            return Ok(());
        }
        Err(DomainError::TextField {
            field: "trace_parent",
            detail: event_broker_sdk::validate::TRACE_PARENT_RULE.to_owned(),
            reason: event_broker_sdk::error::reasons::INVALID_EVENT_FIELD,
        })
    }

    /// The request's size as the per-event limit counts it: the bytes the SDK's
    /// `EventV1::encoded_len` counts for the same event, or fewer, never more.
    /// Parsing makes a `null` `data` of an absent one, so the request cannot tell
    /// those apart, and [`PublishMeta::producer`] reads a literal nil `meta.producer`
    /// as an absent one. Each counts as the shorter of its two forms: `data` left out
    /// (12 bytes fewer than the SDK's `"data":null` for a set `null`) and the producer
    /// id as `null` (34 fewer for a literal nil id).
    ///
    /// # Errors
    ///
    /// [`DomainError::Internal`] when the request does not serialize, which no
    /// `serde_json::Value` payload does: a broken internal invariant.
    pub(crate) fn encoded_len(&self) -> Result<usize, DomainError> {
        let encoded = EncodedEvent {
            id: self.id,
            r#type: &self.r#type,
            subject_tenant: self.subject_tenant,
            source: &self.source,
            subject: &self.subject,
            subject_type: &self.subject_type,
            subject_owner: self.subject_owner,
            occurred_at: self.occurred_at,
            trace_parent: self.trace_parent.as_deref(),
            data: (!self.data.is_null()).then_some(&self.data),
            meta: self.meta.as_ref().map(|meta| EncodedMeta {
                version: meta.version,
                producer: meta.producer(),
                previous: meta.previous,
                sequence: meta.sequence,
            }),
        };
        serde_json::to_vec(&encoded)
            .map(|bytes| bytes.len())
            .map_err(|err| {
                // The class only: a serde message can quote the value.
                DomainError::Internal(format!(
                    "measure an event's encoding ({:?})",
                    err.classify()
                ))
            })
    }

    /// Refuses a request over `EventV1::MAX_ENCODED_BYTES`, naming it as event
    /// `index` of its request.
    ///
    /// # Errors
    ///
    /// [`DomainError::EventTooLarge`] when it encodes to more; the error of
    /// [`encoded_len`](Self::encoded_len).
    pub(crate) fn check_size(&self, index: usize) -> Result<(), DomainError> {
        let bytes = self.encoded_len()?;
        if bytes > EventV1::MAX_ENCODED_BYTES {
            return Err(DomainError::EventTooLarge {
                index,
                bytes,
                max: EventV1::MAX_ENCODED_BYTES,
            });
        }
        Ok(())
    }

    /// Refuses a request whose event tenant is the nil UUID, naming it as event
    /// `index` of its request. Each publish path runs it before
    /// `prepare_publish`, so such a request reaches no PDP call.
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] with [`ErrorCode::InvalidTenant`] when the
    /// tenant is nil.
    pub(crate) fn check_tenant(&self, index: usize) -> Result<(), DomainError> {
        if self.subject_tenant.is_nil() {
            return Err(DomainError::Validation {
                code: ErrorCode::InvalidTenant,
                message: format!("event {index}: the event's tenant must not be the nil UUID"),
            });
        }
        Ok(())
    }

    /// Refuses a request whose owner is the nil UUID, naming it as event `index`
    /// of its request: an event with no owner leaves the member out or `null`.
    /// Each publish path runs it beside [`check_tenant`](Self::check_tenant).
    ///
    /// # Errors
    ///
    /// [`DomainError::Validation`] with [`ErrorCode::InvalidSubjectOwner`] when
    /// the owner is nil.
    pub(crate) fn check_subject_owner(&self, index: usize) -> Result<(), DomainError> {
        if self.subject_owner.is_some_and(|owner| owner.is_nil()) {
            return Err(DomainError::Validation {
                code: ErrorCode::InvalidSubjectOwner,
                message: format!("event {index}: the event's owner must not be the nil UUID"),
            });
        }
        Ok(())
    }
}

/// A [`PublishRequest`] in the shape the SDK's `EventV1` serializes to, borrowed, for
/// [`PublishRequest::encoded_len`]: the same members, each value parsing cannot tell
/// from an absent one in its shorter form.
/// No server-stamped member: an `EventV1` on its way in has none, and skips them.
#[derive(serde::Serialize)]
struct EncodedEvent<'a> {
    id: Uuid,
    r#type: &'a GtsTypeId,
    subject_tenant: Uuid,
    source: &'a str,
    subject: &'a str,
    subject_type: &'a GtsTypeId,
    subject_owner: Option<Uuid>,
    occurred_at: chrono::DateTime<chrono::Utc>,
    trace_parent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<&'a serde_json::Value>,
    meta: Option<EncodedMeta>,
}

/// The `meta` of an [`EncodedEvent`], as the SDK's `EventMeta` serializes.
#[derive(serde::Serialize)]
struct EncodedMeta {
    version: i32,
    producer: Option<Uuid>,
    previous: Option<i64>,
    sequence: Option<i64>,
}

/// Outcome of admitting a batch, all-or-nothing (feature 0001 §5.2): some
/// event was newly enqueued, or every event was a duplicate - which the REST
/// layer answers `200` rather than `202`. A stateless event is never a
/// duplicate, so a batch carrying one is always `Accepted`.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchAck {
    Accepted,
    Duplicate,
}

/// Outcome of admitting one event. Both variants carry the stamped event; the
/// distinction is whether the event was newly admitted or matched an
/// idempotent duplicate - a lost-ack retry of the producer chain's current
/// head - which the REST layer answers `200` (no new row written) rather than
/// `202`.
#[domain_model]
#[derive(Debug, Clone)]
pub enum PublishAck {
    Accepted(Event),
    Duplicate(Event),
}

/// Outcome of a synchronous publish (`Prefer: wait=N`, `DESIGN.md` "Sync vs
/// Async Persist Modes"): the backend stored every admitted event within N
/// (`201 Created`), N elapsed first with the events still enqueued to be stored
/// later (`202 Accepted`), or every event was a duplicate so nothing was
/// enqueued and there was nothing to wait for (`200 OK`). An append failure is
/// retried, never answered, so it surfaces as `TimedOut`, not an error.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPublishAck {
    Stored,
    TimedOut,
    Duplicate,
}

/// Dedup protocol mode a producer registers under (`ADR-0004`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProducerMode {
    Chained,
    Monotonic,
}

/// A synchronous-publish trace: a per-publish UUID the broker mints and the
/// waiting request subscribes to (feature 0007 §4.1). Never on the wire.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncTrace(Uuid);

impl SyncTrace {
    /// A fresh trace. A v4 (random) UUID rather than a time-ordered v7, so new
    /// trace rows do not cluster at the tail of the outbox's non-unique `trace`
    /// index (where an ack's countdown could gap-lock the next insert).
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// The underlying id, for subscribing and for stamping the outbox row.
    #[must_use]
    pub fn id(self) -> Uuid {
        self.0
    }
}

impl Default for SyncTrace {
    fn default() -> Self {
        Self::new()
    }
}

/// How a prepared publish is stamped onto the outbox: an async publish records
/// nothing, a synchronous publish rides a [`SyncTrace`] its waiting request is
/// told the completion of. An enum rather than an `Option<Uuid>` argument, the
/// gear's convention for a parameter with two distinct meanings.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub enum OutboxStamp {
    Async,
    Sync(SyncTrace),
}

impl OutboxStamp {
    /// Whether this is a synchronous publish (the outbox row is flushed
    /// `Flush::Sync` and the batch is enqueued under its trace).
    const fn is_sync(self) -> bool {
        matches!(self, Self::Sync(_))
    }

    /// The trace to enqueue the row under, if any.
    const fn trace(self) -> Option<Uuid> {
        match self {
            Self::Sync(trace) => Some(trace.0),
            Self::Async => None,
        }
    }
}

/// Input to `IngestService::register_producer` - this codebase's convention
/// is a request struct per multi-field operation (`ConsumerGroupCreateInput`,
/// `JoinRequest`), not positional loose args.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProducerRegistrationInput {
    pub mode: ProducerMode,
    pub client_agent: String,
}

/// Result of `IngestService::register_producer` - echoes the identity fields
/// `docs/openapi.yaml`'s `POST /v1/producers` response requires.
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProducerRegistration {
    pub id: Uuid,
    pub mode: ProducerMode,
    pub client_agent: String,
}

/// A registered producer's identity, tenant and dedup mode, as
/// [`ProducerRegistry::find`] returns it. Folds existence, ownership, tenant
/// and mode into one lookup so the publish path resolves them from a single
/// query, and the cursor and reset paths ask the tenant check about the
/// producer's own tenant without a second read.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct ProducerRecord {
    pub owner: Uuid,
    /// The tenant the producer was registered in: the registering caller's.
    pub tenant: Uuid,
    pub mode: ProducerMode,
}

/// A producer's chain on one topic: its `last_sequence`. The chain is kept per
/// `(producer_id, topic)`; no partition is part of it (ADR-0004 "Chain Scope").
#[domain_model]
#[derive(Debug, Clone)]
pub struct ProducerTopicCursors {
    pub topic: GtsInstanceId,
    pub last_sequence: i64,
}

/// Result of `IngestService::get_producer_cursors` (`docs/openapi.yaml`'s
/// `GET /v1/producers/{id}/cursors` response) - an object (not a bare
/// array) so the echoed identity fields ride along and the shape stays
/// extensible.
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct ProducerCursors {
    pub producer_id: Uuid,
    pub client_agent: String,
    pub topics: Vec<ProducerTopicCursors>,
}

/// Which items of a metadata list the caller asked for. A list applies it
/// before it authorizes each item, so an item the caller did not ask for
/// costs no PDP call: REST passes its `$filter`, in-process callers pass
/// [`Selection::all`].
pub struct Selection<'a, T> {
    keep: Option<Keep<'a, T>>,
}

/// The predicate a [`Selection`] keeps an item by.
type Keep<'a, T> = &'a (dyn Fn(&T) -> bool + Send + Sync);

impl<'a, T> Selection<'a, T> {
    /// Every item.
    #[must_use]
    pub fn all() -> Self {
        Self { keep: None }
    }

    /// The items `keep` returns `true` for.
    #[must_use]
    pub fn only(keep: Keep<'a, T>) -> Self {
        Self { keep: Some(keep) }
    }

    /// Whether `item` is selected.
    #[must_use]
    pub fn selects(&self, item: &T) -> bool {
        self.keep.is_none_or(|keep| keep(item))
    }
}

impl<T> std::fmt::Debug for Selection<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selection")
            .field("all", &self.keep.is_none())
            .finish()
    }
}

#[async_trait]
pub trait IngestService: Send + Sync {
    /// Validate, sequence, and enqueue one event for persistence
    /// (`DESIGN.md` "`IngestService`").
    async fn publish_event(
        &self,
        ctx: &SecurityContext,
        request: PublishRequest,
    ) -> Result<PublishAck, DomainError>;

    /// Validate and enqueue a batch of events whose event types all resolve to
    /// the same topic (feature 0001 §5.2). Events name no topic themselves, so
    /// batch homogeneity is a property of the resolved types, not of a field
    /// the producer set.
    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        requests: Vec<PublishRequest>,
    ) -> Result<BatchAck, DomainError>;

    /// [`publish_event`](Self::publish_event), held until the backend stores
    /// the event or `wait` elapses (`Prefer: wait=N`, `DESIGN.md` "Sync vs
    /// Async Persist Modes"). `wait` is the requested hold, clamped to the
    /// broker's configured ceiling; pass `Duration::MAX` to hold for as long as
    /// the broker allows. A duplicate is answered at once, never waited on.
    async fn publish_event_sync(
        &self,
        ctx: &SecurityContext,
        request: PublishRequest,
        wait: std::time::Duration,
    ) -> Result<SyncPublishAck, DomainError>;

    /// [`publish_batch`](Self::publish_batch), held until the backend stores
    /// every admitted event of the batch or `wait` elapses. The hold applies
    /// to the batch as a whole and is clamped like
    /// [`publish_event_sync`](Self::publish_event_sync); an all-duplicate batch
    /// is answered at once.
    async fn publish_batch_sync(
        &self,
        ctx: &SecurityContext,
        requests: Vec<PublishRequest>,
        wait: std::time::Duration,
    ) -> Result<SyncPublishAck, DomainError>;

    /// The ceiling a `Prefer: wait=N` hold is clamped to
    /// (`config::PublishConfig::max_wait_secs`). The REST layer reports the
    /// applied wait with `Preference-Applied: wait=<seconds>` (RFC 7240 §3), so
    /// a client can tell a synchronous `202` from a preference the broker
    /// ignored; the value is whole seconds, as the header carries.
    fn max_sync_wait(&self) -> std::time::Duration;

    /// Mint a fresh `producer_id` bound to `ctx`'s calling principal
    /// (`docs/openapi.yaml`'s `POST /v1/producers`). The `client_agent` is
    /// checked first, then the tenant check (`produce` at the caller's own
    /// tenant), then the producer is stored.
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        input: ProducerRegistrationInput,
    ) -> Result<ProducerRegistration, DomainError>;

    /// Read a producer's per-topic `last_sequence` cursors.
    /// Principal-bound - only the registering principal may call this
    /// (`docs/openapi.yaml`'s `GET /v1/producers/{id}/cursors`). The tenant
    /// check (`produce` at the producer's own tenant) runs before the owner
    /// check.
    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: Uuid,
    ) -> Result<ProducerCursors, DomainError>;

    /// Operator-driven chain reset. Principal-bound - only the registering
    /// principal may call this (`docs/openapi.yaml`'s
    /// `POST /v1/producers/{id}:reset`). The tenant check (`produce` at the
    /// producer's own tenant) runs before the owner check.
    async fn reset_producer(
        &self,
        ctx: &SecurityContext,
        producer_id: Uuid,
        scope: ProducerResetScope,
    ) -> Result<(), DomainError>;

    /// `GET /v1/topics`: the registered topics `select` keeps that the caller
    /// may read ([`MetadataAccess::may_read_topic`]).
    async fn list_topics(
        &self,
        ctx: &SecurityContext,
        select: &Selection<'_, crate::domain::model::Topic>,
    ) -> Result<Vec<crate::domain::model::Topic>, DomainError>;

    /// `GET /v1/topics/segments`, for a topic the caller may read
    /// ([`MetadataAccess::may_read_topic`]); a refused caller gets
    /// `TopicNotAuthorized` whether or not the topic is registered.
    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &GtsInstanceId,
        partition: i32,
    ) -> Result<crate::domain::model::TopicSegmentManifest, DomainError>;

    /// `GET /v1/event-types`: the registered event types `select` keeps that
    /// the caller may read: it may consume the type's topic, or else publish
    /// the type.
    async fn list_event_types(
        &self,
        ctx: &SecurityContext,
        select: &Selection<'_, EventType>,
    ) -> Result<Vec<EventType>, DomainError>;

    /// One registered event type the caller may read, as
    /// [`list_event_types`](Self::list_event_types) decides it; `None` when
    /// `id` is not registered or the caller may not read it.
    async fn get_event_type(
        &self,
        ctx: &SecurityContext,
        id: &GtsTypeId,
    ) -> Result<Option<EventType>, DomainError>;
}

/// Producer identity/ownership bookkeeping - separate from the
/// per-`(producer_id, topic)` sequence state (`IdempotencyGuard` owns that)
/// because registration/ownership/client-agent are keyed purely by
/// `producer_id`, with no topic dimension.
#[async_trait]
pub trait ProducerRegistry: Send + Sync {
    /// `tenant_id` is captured from `ctx.subject_tenant_id()` at the call site, never
    /// overridable - matches `ConsumerGroup.tenant_id`'s own "non-overridable,
    /// captured from `SecurityContext`" convention.
    async fn register(
        &self,
        owner: Uuid,
        tenant_id: Uuid,
        mode: ProducerMode,
        client_agent: String,
    ) -> Result<ProducerRegistration, DomainError>;

    /// The producer's identity and mode, or `Ok(None)` if `producer_id` was
    /// never registered. One lookup serves both the existence/ownership checks
    /// and the publish path's need for the registered mode. `Err` is possible
    /// (unlike the in-memory stand-in this replaces) since a real SQL-backed
    /// implementation can fail on infrastructure grounds -
    /// eb-single-process-implementation made every `ProducerRegistry` method
    /// fallible for the same reason every other domain repo trait already
    /// is.
    async fn find(&self, producer_id: Uuid) -> Result<Option<ProducerRecord>, DomainError>;

    /// `Ok(None)` if `producer_id` was never registered (distinct from a
    /// registered producer with no cursors yet, which returns
    /// `Ok(Some(ProducerCursors { topics: vec![], .. }))`).
    async fn cursors(&self, producer_id: Uuid) -> Result<Option<ProducerCursors>, DomainError>;

    async fn reset(&self, producer_id: Uuid, scope: &ProducerResetScope)
    -> Result<(), DomainError>;
}

/// Real `IngestService`: topic/event-type resolution via
/// `SpecificationManager`, partition-key resolution, and idempotency plus
/// durable enqueue via `IdempotencyGuard`; the backend append happens when the
/// ingest outbox drains. Generic over one repo type implementing the traits
/// it needs (idempotency, producer registry - neither topics nor events have
/// a repo contract).
#[domain_model]
pub struct IngestServiceImpl<R> {
    repo: std::sync::Arc<R>,
    policy_enforcer: PolicyEnforcer,
    spec_manager: std::sync::Arc<dyn crate::domain::specification::SpecificationManager>,
    backend_resolver: std::sync::Arc<dyn BackendResolver>,
    batch: crate::config::BatchConfig,
    /// The `default` outbox group's partitions: what an event's ingest
    /// partition is taken modulo, so it has to be the count the group's queue
    /// was registered with.
    outbox_partitions: crate::domain::outbox::OutboxPartitions,
    /// How a synchronous publish learns its events were stored: it subscribes
    /// to the publish's outbox trace before the enqueue commits and awaits its
    /// completion (`domain::sync_wait::SyncWaitRegistry`, feature 0007 §4.1).
    /// Cross-instance through the outbox's own notifier, so a publish whose rows
    /// drain on another instance still resolves.
    sync_wait: std::sync::Arc<dyn crate::domain::sync_wait::SyncWaitRegistry>,
    /// The ceiling a `Prefer: wait=N` hold is clamped to
    /// (`config::PublishConfig::max_wait_secs`): the broker never holds a
    /// publish open longer than the operator allows, so the service that does
    /// the waiting owns the cap rather than each caller carrying it.
    sync_wait_max: std::time::Duration,
}

impl<R> IngestServiceImpl<R> {
    /// The topic an event type publishes to, as the type's `topic` trait names
    /// it. An event carries only its type, so this is the only route from a
    /// publish request to a stream.
    ///
    /// # Errors
    ///
    /// `DomainError::NotFound` with `EventTypeNotFound` when the type is not
    /// registered; `DomainError::StorageUnavailable` when it cannot be read.
    async fn resolve_topic(&self, event_type: &GtsTypeId) -> Result<GtsInstanceId, DomainError> {
        self.spec_manager
            .get_event_type(event_type)
            .await?
            .map(|resolved| resolved.topic)
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::EventTypeNotFound,
                message: format!("event type '{event_type}' is not registered"),
                resource: event_type.to_string(),
            })
    }

    /// The requested synchronous-publish hold, never beyond the broker's
    /// configured ceiling (`Prefer: wait=N` is capped by configuration).
    fn clamp_wait(&self, wait: std::time::Duration) -> std::time::Duration {
        wait.min(self.sync_wait_max)
    }

    /// The metadata-read checks, asked as `ctx`.
    fn metadata_access(&self, ctx: &SecurityContext) -> MetadataAccess {
        MetadataAccess::new(ctx, &self.policy_enforcer, &self.spec_manager)
    }

    /// Authorizes one publish (#5492). A non-event type is refused first, so it
    /// gets `404` before any PDP call. Then, in order:
    ///
    /// - the tenant check: `produce` on the tenant-scope resource, rooted at
    ///   the event's tenant with barriers respected;
    /// - only when the tenant check is denied, the barrier right:
    ///   `produce_ignore_barrier` on the event type, rooted at the event's
    ///   tenant with barriers ignored;
    /// - the type check: `produce` on the event type, rooted at the caller's
    ///   own tenant, with the event's tenant, subject type and owner (when it has
    ///   one) as resource properties.
    ///
    /// A tenant check that fails to evaluate is an outage, not a denial, so it
    /// is returned as is and never retried as the barrier right.
    ///
    /// # Errors
    ///
    /// `DomainError::NotFound` (`EventTypeNotFound`) for a non-event type;
    /// `DomainError::Forbidden` with `TenantIdNotAuthorized` when the tenant
    /// check and the barrier right are both denied, or with
    /// `NotAuthorizedToProduce` when the type check is denied;
    /// `DomainError::Internal` when an evaluation fails.
    async fn authorize_publish(
        &self,
        ctx: &SecurityContext,
        request: &PublishRequest,
    ) -> Result<(), DomainError> {
        let event_type_resource = event_type_resource(&request.r#type)?;
        let caller_tenant = ctx.subject_tenant_id();
        let event_tenant = request.subject_tenant;

        match self
            .policy_enforcer
            .access_scope_with(
                ctx,
                &TENANT_SCOPE_RESOURCE,
                "produce",
                None,
                &Self::reach(event_tenant, BarrierMode::Respect),
            )
            .await
        {
            Ok(_) => {}
            Err(EnforcerError::AccessDenied(_)) => {
                self.policy_enforcer
                    .access_scope_with(
                        ctx,
                        &event_type_resource,
                        "produce_ignore_barrier",
                        None,
                        &Self::reach(event_tenant, BarrierMode::Ignore)
                            .resource_property("event_type_id", request.r#type.as_ref()),
                    )
                    .await
                    .map_err(|e| with_forbidden_code(e.into(), ErrorCode::TenantIdNotAuthorized))?;
            }
            Err(error) => return Err(error.into()),
        }

        let mut type_check = AccessRequest::new()
            .context_tenant_id(caller_tenant)
            .tenant_mode(TenantMode::RootOnly)
            .resource_property("event_type_id", request.r#type.as_ref())
            .resource_property(pep_properties::OWNER_TENANT_ID, event_tenant)
            .resource_property(EVENT_SUBJECT_TYPE, request.subject_type.as_ref())
            .require_constraints(false);
        if let Some(owner) = request.subject_owner {
            type_check = type_check.resource_property(pep_properties::OWNER_ID, owner);
        }
        self.policy_enforcer
            .access_scope_with(ctx, &event_type_resource, "produce", None, &type_check)
            .await
            .map_err(|e| with_forbidden_code(e.into(), ErrorCode::NotAuthorizedToProduce))?;
        Ok(())
    }

    /// The tenant context and owner a publish's reach requests share: rooted
    /// at the event's tenant alone, with the given barrier mode.
    fn reach(event_tenant: Uuid, barrier_mode: BarrierMode) -> AccessRequest {
        AccessRequest::new()
            .context_tenant_id(event_tenant)
            .tenant_mode(TenantMode::RootOnly)
            .barrier_mode(barrier_mode)
            .resource_property(pep_properties::OWNER_TENANT_ID, event_tenant)
            .require_constraints(false)
    }

    // Each argument is a distinct, fully-required dependency and this is the
    // single construction site (via `build_handler_state`), so a deps-struct
    // would add indirection without preventing any real mistake.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is a distinct fully-required dependency, built at one site"
    )]
    #[must_use]
    pub fn new(
        repo: std::sync::Arc<R>,
        policy_enforcer: PolicyEnforcer,
        spec_manager: std::sync::Arc<dyn crate::domain::specification::SpecificationManager>,
        backend_resolver: std::sync::Arc<dyn BackendResolver>,
        batch: crate::config::BatchConfig,
        outbox_partitions: crate::domain::outbox::OutboxPartitions,
        sync_wait: std::sync::Arc<dyn crate::domain::sync_wait::SyncWaitRegistry>,
        sync_wait_max: std::time::Duration,
    ) -> Self {
        Self {
            repo,
            policy_enforcer,
            spec_manager,
            backend_resolver,
            batch,
            outbox_partitions,
            sync_wait,
            sync_wait_max,
        }
    }
}

/// One validated event, ready for the chain check and the enqueue: the
/// stamped event and what the idempotency guard enqueues for it.
struct PreparedPublish {
    event: Event,
    enqueue: crate::domain::idempotency::PublishEnqueue,
}

impl<R> IngestServiceImpl<R>
where
    R: ProducerRegistry + Send + Sync + 'static,
{
    /// Everything a publish checks before its chain is: authorization, the
    /// event and subject types, the topic and its provisioning, the producer
    /// and its `meta` shape, the payload schema; then the stamped event and its
    /// ingest partition.
    /// Writes nothing, so a batch prepares every event before enqueueing any.
    /// `index` is the request's position in its batch (`0` for a single publish),
    /// which a refused partition key names.
    async fn prepare_publish(
        &self,
        ctx: &SecurityContext,
        mut request: PublishRequest,
        index: usize,
        stamp: OutboxStamp,
    ) -> Result<PreparedPublish, DomainError> {
        // Authorize before topic/schema validation (`docs/DESIGN.md`,
        // Validation Pipeline step 1 and §3.3).
        self.authorize_publish(ctx, &request).await?;

        // NOTE(perf): most first-level fields are deterministic per type (a few
        // are type-overridden), so they could be checked without full schema
        // validation.
        let event_type = self
            .spec_manager
            .get_event_type(&request.r#type)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::EventTypeNotFound,
                message: format!("event type '{}' is not registered", request.r#type),
                resource: request.r#type.to_string(),
            })?;

        // The event type names the stream, so the type lookup above is also
        // the topic lookup: nothing the producer sent can point elsewhere.
        let topic = self
            .spec_manager
            .get_topic(&event_type.topic)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::TopicNotFound,
                message: format!("topic '{}' is not registered", event_type.topic),
                resource: event_type.topic.to_string(),
            })?;

        subject_type_allowed(&event_type, request.subject_type.as_ref())?;

        // A topic its backend does not hold yet cannot take events: answered
        // `503`, retryable, until provisioning completes (feature 0006 §4.2).
        if !matches!(
            self.spec_manager.topic_state(&topic).await?,
            Some(crate::domain::specification::TopicState::Provisioned { .. })
        ) {
            return Err(DomainError::StorageUnavailable {
                reason: format!(
                    "topic '{}' is not provisioned on its storage backend yet",
                    topic.id
                ),
                source: None,
            });
        }

        // The `meta` block, in the order ADR-0004 gives ("Mode-Shape Enforcement
        // at Publish Time"). A block naming no producer is refused when it carries
        // a chain field, then when its version is newer than supported; otherwise
        // it is a stateless publish's, stored as if it were absent. A block naming
        // one is resolved first: an unknown producer - never registered, or since
        // deleted - is a `404 ProducerNotFound` naming the producer, which the
        // producer's own outbox recovers into a re-registration. The registered
        // mode then decides the shape and which rule the chain check below
        // applies, and the version is checked last.
        let chain = match request.meta.take() {
            None => None,
            Some(meta) => match meta.producer() {
                None => {
                    meta.check_without_producer()?;
                    meta.check_version()?;
                    None
                }
                Some(producer_id) => {
                    let record = self.repo.find(producer_id).await?.ok_or_else(|| {
                        DomainError::NotFound {
                            code: ErrorCode::ProducerNotFound,
                            message:
                                "the producer is not registered or its registration has expired"
                                    .to_owned(),
                            resource: producer_id.to_string(),
                        }
                    })?;
                    Some((meta.into_stored(producer_id, record.mode)?, record.mode))
                }
            },
        };

        self.spec_manager
            .validate_event_data(&event_type, &request.data)
            .await?;

        // The routing contract is the event type's, not this publisher's: the
        // type names a member of the event with a JSON Pointer, and the value
        // at it is what the backend partitions the event by. Ingest assigns no
        // topic partition: it only places the event in the ingest outbox
        // partition of its key, so events of one key drain in publish order.
        let key = crate::domain::backend::partition_input(&request, &event_type.partition_key)?;
        // The key is stored beside the event in the ingest outbox row, so its cap keeps
        // the row within the outbox's limit (`EventV1::OUTBOX_RESERVE_BYTES`).
        let key_bytes = EventV1::partition_key_len(&key).map_err(|err| {
            DomainError::Internal(format!("measure a partition key: {}", err.kind()))
        })?;
        if key_bytes > EventV1::MAX_PARTITION_KEY_BYTES {
            return Err(DomainError::PartitionKeyTooLong {
                index,
                bytes: key_bytes,
                max: EventV1::MAX_PARTITION_KEY_BYTES,
            });
        }
        let ingest_partition =
            crate::domain::outbox::outbox_partition_for(&topic.id, &key, self.outbox_partitions);

        // Partition, sequence and sequence time are the backend's, assigned
        // when the outbox drains the event; none is known here.
        let event = Event {
            id: request.id,
            r#type: request.r#type,
            topic: topic.id.clone(),
            subject_tenant: request.subject_tenant,
            source: request.source,
            subject: request.subject,
            subject_type: request.subject_type,
            subject_owner: request.subject_owner,
            occurred_at: request.occurred_at,
            trace_parent: request.trace_parent,
            data: request.data,
            meta: chain.as_ref().map(|(meta, _)| meta.clone()),
            ingest: IngestStamp::of_caller(ctx),
            partition: None,
            sequence: None,
            sequence_time: None,
        };
        let payload = serde_json::to_vec(&crate::domain::outbox::IngestPayload {
            topic: topic.id.clone(),
            key,
            event: event.clone(),
            // A sync row is flushed `Flush::Sync` by the worker so a `201`'s
            // event is immediately readable; the wait itself rides the outbox
            // trace below, not the payload.
            stored_wait: stamp.is_sync(),
        })
        .map_err(|e| DomainError::Internal(format!("serialize ingest outbox payload: {e}")))?;

        let mut request = crate::domain::idempotency::PublishEnqueue::builder(
            ingest_partition,
            payload,
            crate::domain::outbox::INGEST_PAYLOAD_TYPE,
        );
        // Enqueue under the publish's trace, so the admitted rows become one
        // traced outbox batch the waiting publish is told the completion of.
        if let Some(trace) = stamp.trace() {
            request = request.trace(trace);
        }
        if let Some((meta, mode)) = chain {
            request = request.chain(crate::domain::idempotency::ProducerChainCheck {
                producer_id: meta.producer_id,
                topic: topic.id.clone(),
                // A chained producer's `meta` always carries `previous`
                // (`PublishMeta::into_stored`); a monotonic one's never does,
                // and its check ignores this field.
                previous: meta.previous.unwrap_or(0),
                sequence: meta.sequence,
                mode,
            });
        }

        Ok(PreparedPublish {
            event,
            enqueue: request.build(),
        })
    }

    /// Everything a batch publish checks and prepares before any row is
    /// written (feature 0001 §5.2): the size and payload bounds, each
    /// event's tenant, the repeated-id and single-topic rules, each event's
    /// own preparation (stamped with `trace`), and each run's shape (R3).
    /// Writes nothing, so the caller runs the one enqueue transaction. Shared
    /// by the async and synchronous batch paths.
    async fn prepare_batch(
        &self,
        ctx: &SecurityContext,
        requests: Vec<PublishRequest>,
        stamp: OutboxStamp,
    ) -> Result<Vec<crate::domain::idempotency::PublishEnqueue>, DomainError> {
        let max = self.batch.max_size as usize;
        if requests.len() > max {
            return Err(DomainError::BatchTooLarge {
                count: requests.len(),
                max,
            });
        }
        // Feature 0001 §5.2 R1.
        if requests.is_empty() {
            return Err(DomainError::EmptyBatch);
        }
        // Feature 0001 §5.2 R13, decided on the request as sent, before it is
        // split into runs.
        if let Some(index) = first_repeated_event_id(&requests) {
            return Err(DomainError::DuplicateEventId { index });
        }

        // The payload byte bound is measured from the requests' own data. The
        // length is a derived measurement, not user content, so reporting it in
        // the error is safe.
        let payload_bytes: u64 = requests
            .iter()
            .map(|request| serde_json::to_vec(&request.data).map(|encoded| encoded.len() as u64))
            .sum::<Result<u64, _>>()
            .map_err(|err| {
                DomainError::Internal(format!("batch payload measurement failed: {err}"))
            })?;
        if payload_bytes > self.batch.max_payload_bytes {
            return Err(DomainError::BatchPayloadTooLarge {
                bytes: payload_bytes,
                max: self.batch.max_payload_bytes,
            });
        }
        for (index, request) in requests.iter().enumerate() {
            request.check_size(index)?;
            request.check_tenant(index)?;
            request.check_subject_owner(index)?;
        }

        // A batch is atomic per topic, and a request names only its event
        // type, so homogeneity is decided by resolving each type to the topic
        // its `topic` trait names and comparing those. Two requests naming the
        // same topic through different types are one batch; two naming
        // different topics are rejected whatever else they have in common.
        let mut batch_topic: Option<GtsInstanceId> = None;
        for request in &requests {
            let topic = self.resolve_topic(&request.r#type).await?;
            match &batch_topic {
                None => batch_topic = Some(topic),
                Some(first) if *first == topic => {}
                Some(_) => {
                    return Err(DomainError::Validation {
                        code: ErrorCode::MixedTopics,
                        message: "a batch must target a single topic".to_owned(),
                    });
                }
            }
        }

        // All-or-nothing: every event is validated before any is enqueued, and
        // the enqueue is one transaction that a chain violation rolls back.
        let mut enqueues = Vec::with_capacity(requests.len());
        for (index, request) in requests.into_iter().enumerate() {
            let PreparedPublish { enqueue, .. } =
                self.prepare_publish(ctx, request, index, stamp).await?;
            enqueues.push(enqueue);
        }
        // Feature 0001 §5.2 R3: each run's own shape, before any chain state
        // is read. It needs each producer's mode, which preparing resolved. A
        // single publish is a run of one that R3 does not gate; its chain
        // decision refuses a non-advancing link on its own.
        for run in ChainRun::split(&enqueues) {
            if let Some(index) = run.first_malformed() {
                return Err(DomainError::MalformedRun { index });
            }
        }
        Ok(enqueues)
    }
}

#[async_trait]
impl<R> IngestService for IngestServiceImpl<R>
where
    R: crate::domain::idempotency::IdempotencyGuard + ProducerRegistry + Send + Sync + 'static,
{
    async fn publish_event(
        &self,
        ctx: &SecurityContext,
        request: PublishRequest,
    ) -> Result<PublishAck, DomainError> {
        // A single publish is a run of one, so a refusal names index 0.
        request.check_size(0)?;
        request.check_tenant(0)?;
        request.check_subject_owner(0)?;
        let PreparedPublish { event, enqueue } = self
            .prepare_publish(ctx, request, 0, OutboxStamp::Async)
            .await?;

        // `check_and_enqueue` is the one DB transaction spanning the
        // producer-chain check and the outbox insert - the backend append
        // happens later, out of transaction, when the outbox processor
        // drains the row.
        match self.repo.check_and_enqueue(enqueue).await? {
            ProducerIdempotencyOutcome::Accept => {}
            ProducerIdempotencyOutcome::DuplicateIgnore => {
                return Ok(PublishAck::Duplicate(event));
            }
            // A single publish is a run of one (feature 0001 §5.2), so its
            // violation names index 0.
            ProducerIdempotencyOutcome::SequenceViolation {
                producer_id,
                last_sequence,
            } => {
                return Err(DomainError::SequenceViolation {
                    index: 0,
                    producer_id,
                    topic: event.topic.to_string(),
                    last_sequence,
                });
            }
        }

        Ok(PublishAck::Accepted(event))
    }

    async fn publish_batch(
        &self,
        ctx: &SecurityContext,
        requests: Vec<PublishRequest>,
    ) -> Result<BatchAck, DomainError> {
        let enqueues = self
            .prepare_batch(ctx, requests, OutboxStamp::Async)
            .await?;
        let expected = enqueues.len();
        let outcomes = self.repo.check_and_enqueue_batch(enqueues).await?;
        if outcomes.len() != expected {
            return Err(DomainError::Internal(format!(
                "the batch enqueue answered {} outcomes for {expected} events",
                outcomes.len(),
            )));
        }
        // Every event a duplicate -> `200` (feature 0001 §5.2 "Outcome"). The
        // pseudo-code's `last_seen_at` touch on `event_broker_producer` is not
        // made: the column is not yet implemented (tracked in #5651).
        if outcomes
            .iter()
            .all(|outcome| *outcome == ProducerIdempotencyOutcome::DuplicateIgnore)
        {
            Ok(BatchAck::Duplicate)
        } else {
            Ok(BatchAck::Accepted)
        }
    }

    async fn publish_event_sync(
        &self,
        ctx: &SecurityContext,
        request: PublishRequest,
        wait: std::time::Duration,
    ) -> Result<SyncPublishAck, DomainError> {
        let wait = self.clamp_wait(wait);
        // A fresh per-publish trace: the row is enqueued under it and the outbox
        // tells us once it is stored (feature 0007 §4.1).
        let trace = SyncTrace::new();
        // A single publish is a run of one, so a refusal names index 0.
        request.check_size(0)?;
        request.check_tenant(0)?;
        request.check_subject_owner(0)?;
        let PreparedPublish { event, enqueue } = self
            .prepare_publish(ctx, request, 0, OutboxStamp::Sync(trace))
            .await?;

        // Subscribe before the enqueue commits, so the completion can never be
        // missed; a duplicate (no row enqueued) drops it with no statement.
        let waiter = self.sync_wait.subscribe(trace);
        match self.repo.check_and_enqueue(enqueue).await? {
            ProducerIdempotencyOutcome::Accept => {}
            // A duplicate wrote no row and is already durable: `200`, no wait.
            ProducerIdempotencyOutcome::DuplicateIgnore => return Ok(SyncPublishAck::Duplicate),
            ProducerIdempotencyOutcome::SequenceViolation {
                producer_id,
                last_sequence,
            } => {
                return Err(DomainError::SequenceViolation {
                    index: 0,
                    producer_id,
                    topic: event.topic.to_string(),
                    last_sequence,
                });
            }
        }

        Ok(if waiter.stored(wait).await {
            SyncPublishAck::Stored
        } else {
            SyncPublishAck::TimedOut
        })
    }

    async fn publish_batch_sync(
        &self,
        ctx: &SecurityContext,
        requests: Vec<PublishRequest>,
        wait: std::time::Duration,
    ) -> Result<SyncPublishAck, DomainError> {
        let wait = self.clamp_wait(wait);
        let trace = SyncTrace::new();
        let enqueues = self
            .prepare_batch(ctx, requests, OutboxStamp::Sync(trace))
            .await?;
        let expected = enqueues.len();

        // Subscribe before the enqueue commits, so the batch's completion can
        // never be missed; an all-duplicate batch drops it with no statement.
        let waiter = self.sync_wait.subscribe(trace);
        let outcomes = self.repo.check_and_enqueue_batch(enqueues).await?;
        if outcomes.len() != expected {
            return Err(DomainError::Internal(format!(
                "the batch enqueue answered {} outcomes for {expected} events",
                outcomes.len(),
            )));
        }
        // Only admitted events are enqueued under the trace, so only they are
        // waited on; an all-duplicate batch admits none (no traced batch is
        // enqueued) and is answered `200` at once (feature 0001 §5.2
        // "Outcome").
        let admitted = outcomes
            .iter()
            .filter(|outcome| **outcome == ProducerIdempotencyOutcome::Accept)
            .count();
        if admitted == 0 {
            return Ok(SyncPublishAck::Duplicate);
        }

        Ok(if waiter.stored(wait).await {
            SyncPublishAck::Stored
        } else {
            SyncPublishAck::TimedOut
        })
    }

    fn max_sync_wait(&self) -> std::time::Duration {
        self.sync_wait_max
    }

    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        input: ProducerRegistrationInput,
    ) -> Result<ProducerRegistration, DomainError> {
        // Validated before the registration is written, on every path in (REST
        // and the in-process broker): this `client_agent` is the one that
        // persists, into a column documented as printable ASCII within 256
        // bytes.
        event_broker_sdk::validate::client_agent(&input.client_agent)?;
        // No caller-identity guard yet: the nil-tenant producers of bss-pricing
        // and bss-products register at start (#5955, #5956), and a guard would
        // refuse them even under a PDP that allows them. The shipped PDPs
        // refuse a nil tenant here on their own.
        tenant_authorized(
            ctx,
            &self.policy_enforcer,
            "produce",
            ctx.subject_tenant_id(),
        )
        .await?;
        self.repo
            .register(
                ctx.subject_id(),
                ctx.subject_tenant_id(),
                input.mode,
                input.client_agent,
            )
            .await
    }

    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: Uuid,
    ) -> Result<ProducerCursors, DomainError> {
        let record = self
            .repo
            .find(producer_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::ProducerNotFound,
                message: format!("producer '{producer_id}' is not registered"),
                resource: producer_id.to_string(),
            })?;
        tenant_authorized(ctx, &self.policy_enforcer, "produce", record.tenant).await?;
        if record.owner != ctx.subject_id() {
            return Err(DomainError::Forbidden {
                code: ErrorCode::ProducerNotOwned,
                message: "calling principal does not own this producer_id".to_owned(),
                resource: producer_id.to_string(),
            });
        }
        self.repo
            .cursors(producer_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::ProducerNotFound,
                message: format!("producer '{producer_id}' is not registered"),
                resource: producer_id.to_string(),
            })
    }

    async fn reset_producer(
        &self,
        ctx: &SecurityContext,
        producer_id: Uuid,
        scope: ProducerResetScope,
    ) -> Result<(), DomainError> {
        let record = self
            .repo
            .find(producer_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::ProducerNotFound,
                message: format!("producer '{producer_id}' is not registered"),
                resource: producer_id.to_string(),
            })?;
        tenant_authorized(ctx, &self.policy_enforcer, "produce", record.tenant).await?;
        if record.owner != ctx.subject_id() {
            return Err(DomainError::Forbidden {
                code: ErrorCode::ProducerNotOwned,
                message: "calling principal does not own this producer_id".to_owned(),
                resource: producer_id.to_string(),
            });
        }
        self.repo.reset(producer_id, &scope).await?;
        audit::emit_producer_chain_reset(ctx, producer_id, &scope);
        Ok(())
    }

    async fn list_topics(
        &self,
        ctx: &SecurityContext,
        select: &Selection<'_, crate::domain::model::Topic>,
    ) -> Result<Vec<crate::domain::model::Topic>, DomainError> {
        let mut topics = self.spec_manager.list_topics().await?;
        topics.retain(|topic| select.selects(topic));
        let access = self.metadata_access(ctx);
        let readable = MetadataAccess::decide_each(&topics, |topic| {
            let access = access.clone();
            let topic = topic.id.clone();
            async move { access.may_read_topic(&topic).await }
        })
        .await?;
        Ok(topics
            .into_iter()
            .zip(readable)
            .filter_map(|(topic, readable)| readable.then_some(topic))
            .collect())
    }

    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic_id: &GtsInstanceId,
        partition: i32,
    ) -> Result<crate::domain::model::TopicSegmentManifest, DomainError> {
        // Before the lookup, so a refused caller cannot tell a registered
        // topic from a missing one.
        if !self.metadata_access(ctx).may_read_topic(topic_id).await? {
            return Err(DomainError::Forbidden {
                code: ErrorCode::TopicNotAuthorized,
                message: "access denied by policy".to_owned(),
                resource: topic_id.to_string(),
            });
        }
        let topic = self
            .spec_manager
            .get_topic(topic_id)
            .await?
            .ok_or_else(|| DomainError::NotFound {
                code: ErrorCode::TopicNotFound,
                message: format!("topic '{topic_id}' is not registered"),
                resource: topic_id.to_string(),
            })?;
        // Checked here rather than left to the backend: a backend reports an
        // unknown partition as not found, while a partition the topic does not
        // have is a malformed request.
        let partitions = *topic.settings.partitions().value();
        if !(0..partitions).contains(&partition) {
            return Err(DomainError::Validation {
                code: ErrorCode::InvalidPartition,
                message: format!("partition must be in [0, {partitions}) for this topic"),
            });
        }
        let backend = self.backend_resolver.resolve(&topic);
        let at = event_broker_sdk::backend::TopicPartition::new(
            topic.id.clone(),
            event_broker_sdk::backend::Partition::new(partition.cast_unsigned()),
        );
        let segments = match backend
            .query(
                &at,
                event_broker_sdk::models::PartitionRange {
                    start_offset: None,
                    end_offset: None,
                    limit: u32::MAX,
                },
            )
            .await
        {
            // Nothing provisioned yet: the partition holds nothing.
            Err(event_broker_sdk::backend::BackendError::NotProvisioned { .. }) => Vec::new(),
            segments => segments?,
        };
        let Some(segment) = segments.into_iter().next() else {
            return Ok(crate::domain::model::TopicSegmentManifest {
                topic: topic.id,
                partition,
                start_sequence: Sequence::assigned(0),
                end_sequence: Sequence::assigned(0),
                start_time: None,
                end_time: None,
                segments: vec![],
            });
        };
        Ok(crate::domain::model::TopicSegmentManifest {
            topic: topic.id,
            partition,
            start_sequence: segment.start_sequence,
            end_sequence: segment.end_sequence,
            start_time: segment.start_time,
            end_time: segment.end_time,
            segments: segment.segments,
        })
    }

    async fn list_event_types(
        &self,
        ctx: &SecurityContext,
        select: &Selection<'_, EventType>,
    ) -> Result<Vec<EventType>, DomainError> {
        let mut event_types = self.spec_manager.list_event_types().await?;
        event_types.retain(|event_type| select.selects(event_type));
        let access = self.metadata_access(ctx);
        // One consume check per topic, then one produce check per type of a
        // refused topic.
        let mut seen = std::collections::HashSet::new();
        let topics: Vec<GtsInstanceId> = event_types
            .iter()
            .filter(|event_type| seen.insert(&event_type.topic))
            .map(|event_type| event_type.topic.clone())
            .collect();
        let consumable = MetadataAccess::decide_each(&topics, |topic| {
            let access = access.clone();
            let topic = topic.clone();
            async move { access.may_consume(&topic).await }
        })
        .await?;
        let consumable: std::collections::HashSet<GtsInstanceId> = topics
            .into_iter()
            .zip(consumable)
            .filter_map(|(topic, consumable)| consumable.then_some(topic))
            .collect();
        let refused: Vec<GtsTypeId> = event_types
            .iter()
            .filter(|event_type| !consumable.contains(&event_type.topic))
            .map(|event_type| event_type.id.clone())
            .collect();
        let mut producible = MetadataAccess::decide_each(&refused, |event_type| {
            let access = access.clone();
            let event_type = event_type.clone();
            async move { access.may_produce(&event_type).await }
        })
        .await?
        .into_iter();
        // `refused` lists the types of refused topics in list order, so each
        // such type takes the next produce decision.
        event_types.retain(|event_type| {
            consumable.contains(&event_type.topic) || producible.next().unwrap_or(false)
        });
        Ok(event_types)
    }

    async fn get_event_type(
        &self,
        ctx: &SecurityContext,
        id: &GtsTypeId,
    ) -> Result<Option<EventType>, DomainError> {
        let Some(event_type) = self.spec_manager.get_event_type(id).await? else {
            return Ok(None);
        };
        let access = self.metadata_access(ctx);
        if access.may_consume(&event_type.topic).await? || access.may_produce(id).await? {
            Ok(Some(event_type))
        } else {
            Ok(None)
        }
    }
}

/// The request index of the first event whose `id` an earlier event of the
/// batch already carries (feature 0001 §5.2 R13): the repeat is named, not the
/// event it repeats.
fn first_repeated_event_id(requests: &[PublishRequest]) -> Option<usize> {
    let mut seen = std::collections::HashSet::with_capacity(requests.len());
    requests.iter().position(|request| !seen.insert(request.id))
}

/// `event_type.allowed_subject_types` pattern grammar (`DESIGN.md` §3.1) via
/// `gts::GtsId::matches_pattern` - real, segment-aware GTS pattern matching,
/// not a hand-rolled string check. `event.subject_type` is a *type*
/// reference (a `*_type`-named field per the GTS spec's field-naming
/// convention - "the KIND of entity", not one instance of it), so it's
/// validated as a GTS Type id (trailing `~`), not a bare instance id.
/// Callers can trust every entry in `allowed_subject_types` is already a
/// valid `GtsIdPattern` (`domain::specification::validate_allowed_subject_types`
/// enforces this at registration), so a parse failure here is
/// defensive-only, not the primary enforcement mechanism - a still-malformed
/// entry (e.g. data written before that validation shipped) is skipped
/// rather than aborting the whole check.
fn subject_type_allowed(event_type: &EventType, subject_type: &str) -> Result<(), DomainError> {
    let candidate = GtsId::try_new(subject_type).map_err(|_| DomainError::Validation {
        code: ErrorCode::InvalidSubjectType,
        message: "subject_type must be a valid GTS id".to_owned(),
    })?;
    if !candidate.is_type() {
        return Err(DomainError::Validation {
            code: ErrorCode::InvalidSubjectType,
            message: "subject_type must be a GTS type id (trailing '~') - subject_type names a \
                 kind of entity, not one instance of it"
                .to_owned(),
        });
    }
    let allowed = event_type
        .allowed_subject_types
        .iter()
        .any(|pattern| candidate.matches_pattern(pattern));
    if allowed {
        Ok(())
    } else {
        Err(DomainError::Validation {
            code: ErrorCode::SubjectTypeNotAllowed,
            message: format!(
                "subject_type is not in event type '{}''s allowed_subject_types",
                event_type.id
            ),
        })
    }
}

#[cfg(test)]
#[path = "ingest_tests.rs"]
mod ingest_tests;
#[cfg(test)]
#[path = "metadata_authz_tests.rs"]
mod metadata_authz_tests;
#[cfg(test)]
#[path = "publish_authz_tests.rs"]
mod publish_authz_tests;
