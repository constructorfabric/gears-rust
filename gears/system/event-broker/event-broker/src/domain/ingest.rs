//! `IngestService` (`DESIGN.md:623-639`): producer-facing, owns the write
//! path.

use crate::domain::model::Sequence;
use async_trait::async_trait;
use authz_resolver_sdk::{AccessRequest, PolicyEnforcer};
use gts::{GtsId, GtsTypeId};
use toolkit::domain_model;
use toolkit_gts::GtsInstanceId;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::audit;
use crate::domain::authz::{event_type_resource, tenant_authorized};
use crate::domain::backend::BackendResolver;
use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::idempotency::{ChainRun, ProducerIdempotencyOutcome};
use crate::domain::producer_reset::ProducerResetScope;
use event_broker_sdk::models::EventType;

use crate::domain::model::{Event, Meta};

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
    pub tenant_id: Uuid,
    pub source: String,
    pub subject: String,
    pub subject_type: GtsTypeId,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
    pub trace_parent: Option<String>,
    pub data: serde_json::Value,
    pub meta: Option<Meta>,
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

/// Dedup protocol mode a producer registers under (`ADR-0004`).
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProducerMode {
    Chained,
    Monotonic,
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

/// A registered producer's identity and dedup mode, as
/// [`ProducerRegistry::find`] returns it. Folds existence, ownership, and mode
/// into one lookup so the publish path resolves all three from a single query
/// instead of one call for existence and another for mode.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct ProducerRecord {
    pub owner: Uuid,
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

#[async_trait]
pub trait IngestService: Send + Sync {
    /// Validate, sequence, and enqueue one event for persistence
    /// (`DESIGN.md:638`).
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

    /// Mint a fresh `producer_id` bound to `ctx`'s calling principal
    /// (`docs/openapi.yaml`'s `POST /v1/producers`).
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        input: ProducerRegistrationInput,
    ) -> Result<ProducerRegistration, DomainError>;

    /// Read a producer's per-topic `last_sequence` cursors.
    /// Principal-bound - only the registering principal may call this
    /// (`docs/openapi.yaml`'s `GET /v1/producers/{id}/cursors`).
    async fn get_producer_cursors(
        &self,
        ctx: &SecurityContext,
        producer_id: Uuid,
    ) -> Result<ProducerCursors, DomainError>;

    /// Operator-driven chain reset. Principal-bound - only the registering
    /// principal may call this (`docs/openapi.yaml`'s
    /// `POST /v1/producers/{id}:reset`).
    async fn reset_producer(
        &self,
        ctx: &SecurityContext,
        producer_id: Uuid,
        scope: ProducerResetScope,
    ) -> Result<(), DomainError>;

    /// `GET /v1/topics` - shared/read-side, forwarded to ingest per the
    /// dispatcher's classification (`eb-dispatcher-routing`, design.md D2).
    /// Takes `ctx` (the security-context-first convention) and returns
    /// `Result` so a future authorization denial is expressible; a topic is a
    /// global, non-tenant-scoped resource, so there is no per-tenant filtering
    /// to apply here as there is for consumer groups and subscriptions.
    async fn list_topics(
        &self,
        ctx: &SecurityContext,
    ) -> Result<Vec<crate::domain::model::Topic>, DomainError>;

    /// `GET /v1/topics/segments`.
    async fn list_topic_segments(
        &self,
        ctx: &SecurityContext,
        topic: &GtsInstanceId,
        partition: i32,
    ) -> Result<crate::domain::model::TopicSegmentManifest, DomainError>;

    /// `GET /v1/event-types` - same classification and `ctx`/`Result`
    /// rationale as `list_topics`.
    async fn list_event_types(&self, ctx: &SecurityContext) -> Result<Vec<EventType>, DomainError>;
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

    #[must_use]
    pub fn new(
        repo: std::sync::Arc<R>,
        policy_enforcer: PolicyEnforcer,
        spec_manager: std::sync::Arc<dyn crate::domain::specification::SpecificationManager>,
        backend_resolver: std::sync::Arc<dyn BackendResolver>,
        batch: crate::config::BatchConfig,
        outbox_partitions: crate::domain::outbox::OutboxPartitions,
    ) -> Self {
        Self {
            repo,
            policy_enforcer,
            spec_manager,
            backend_resolver,
            batch,
            outbox_partitions,
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
    /// event and subject types, the topic and its provisioning, the producer,
    /// the payload schema; then the stamped event and its ingest partition.
    /// Writes nothing, so a batch prepares every event before enqueueing any.
    async fn prepare_publish(
        &self,
        ctx: &SecurityContext,
        request: PublishRequest,
    ) -> Result<PreparedPublish, DomainError> {
        // Reject non-event types, then authorize before topic/schema validation
        // (`docs/DESIGN.md`, Validation Pipeline step 1 and §3.3).
        let event_type_resource = event_type_resource(&request.r#type)?;
        self.policy_enforcer
            .access_scope_with(
                ctx,
                &event_type_resource,
                "produce",
                None,
                &AccessRequest::new()
                    .resource_property("event_type_id", request.r#type.as_ref())
                    .resource_property(
                        toolkit_security::pep_properties::OWNER_TENANT_ID,
                        request.tenant_id,
                    )
                    .require_constraints(false),
            )
            .await
            .map_err(|e| {
                crate::domain::authz::with_forbidden_code(
                    e.into(),
                    ErrorCode::NotAuthorizedToProduce,
                )
            })?;
        tenant_authorized(ctx, &self.policy_enforcer, "produce", request.tenant_id).await?;

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
            self.spec_manager.topic_state(&topic.id).await?,
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

        // A chained/monotonic publish names a registered producer. Resolving it
        // before the chain check reports an unknown `producer_id` - never
        // registered, or since deleted - as a `404 ProducerNotFound` naming the
        // producer. The producer's own outbox recovers this into a
        // re-registration, so it is the producer resource that is named (not
        // the targeted topic). The same lookup yields the registered `mode`,
        // which the chain check needs to apply the right rule. A stateless
        // publish carries a nil `producer_id` and is not subject to this.
        let producer_mode =
            if let Some(meta) = &request.meta
                && !meta.producer_id.is_nil()
            {
                let record = self.repo.find(meta.producer_id).await?.ok_or_else(|| {
                    DomainError::NotFound {
                        code: ErrorCode::ProducerNotFound,
                        message: "the producer is not registered or its registration has expired"
                            .to_owned(),
                        resource: meta.producer_id.to_string(),
                    }
                })?;

                // `meta.previous` is a chained-mode field. A monotonic producer
                // sending it is a mode-shape violation, rejected here - before the
                // sequence comparison in `check_and_enqueue` - so the otherwise
                // acceptable sequence is not applied either.
                if record.mode == ProducerMode::Monotonic && meta.previous.is_some() {
                    return Err(DomainError::TextField {
                        field: "meta.previous",
                        detail: "must be absent for a monotonic-mode producer".to_owned(),
                        reason: "field_not_allowed",
                    });
                }
                Some(record.mode)
            } else {
                None
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
        let ingest_partition =
            crate::domain::outbox::outbox_partition_for(&topic.id, &key, self.outbox_partitions);

        // Partition, sequence and sequence time are the backend's, assigned
        // when the outbox drains the event; none is known here.
        let event = Event {
            id: request.id,
            r#type: request.r#type,
            topic: topic.id.clone(),
            tenant_id: request.tenant_id,
            source: request.source,
            subject: request.subject,
            subject_type: request.subject_type,
            occurred_at: request.occurred_at,
            trace_parent: request.trace_parent,
            data: request.data,
            meta: request.meta,
            partition: None,
            sequence: None,
            sequence_time: None,
        };
        let payload = serde_json::to_vec(&crate::domain::outbox::IngestPayload {
            topic: topic.id.clone(),
            key,
            event: event.clone(),
        })
        .map_err(|e| DomainError::Internal(format!("serialize ingest outbox payload: {e}")))?;

        let mut request = crate::domain::idempotency::PublishEnqueue::builder(
            ingest_partition,
            payload,
            crate::domain::outbox::INGEST_PAYLOAD_TYPE,
        );
        if let Some(meta) = event.meta.clone() {
            request = request.chain(crate::domain::idempotency::ProducerChainCheck {
                producer_id: meta.producer_id,
                topic: topic.id.clone(),
                previous: meta.previous.unwrap_or(0),
                sequence: meta.sequence,
                // `producer_mode` is set for every non-nil producer resolved
                // above; a nil producer_id is checked as chained.
                mode: producer_mode.unwrap_or(ProducerMode::Chained),
            });
        }

        Ok(PreparedPublish {
            event,
            enqueue: request.build(),
        })
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
        let PreparedPublish { event, enqueue } = self.prepare_publish(ctx, request).await?;

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
        let mut events = Vec::with_capacity(requests.len());
        let mut enqueues = Vec::with_capacity(requests.len());
        for request in requests {
            let PreparedPublish { event, enqueue } = self.prepare_publish(ctx, request).await?;
            events.push(event);
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
        let outcomes = self.repo.check_and_enqueue_batch(enqueues).await?;
        if outcomes.len() != events.len() {
            return Err(DomainError::Internal(format!(
                "the batch enqueue answered {} outcomes for {} events",
                outcomes.len(),
                events.len()
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
        _ctx: &SecurityContext,
    ) -> Result<Vec<crate::domain::model::Topic>, DomainError> {
        // Topics are global, non-tenant-scoped resources, so there is no
        // per-tenant read filtering to apply (unlike consumer groups /
        // subscriptions). `ctx` is accepted for the security-context-first
        // convention and to keep a denial expressible; the specification
        // manager it reads from is the ctx-less Tier-1 boundary (it also
        // serves ctx-less retention/refresh workers).
        self.spec_manager.list_topics().await
    }

    async fn list_topic_segments(
        &self,
        _ctx: &SecurityContext,
        topic_id: &GtsInstanceId,
        partition: i32,
    ) -> Result<crate::domain::model::TopicSegmentManifest, DomainError> {
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
        _ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, DomainError> {
        // Same rationale as `list_topics`: event types are global resources,
        // read from the ctx-less Tier-1 specification manager.
        self.spec_manager.list_event_types().await
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
