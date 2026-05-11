use std::sync::Arc;

use chrono::Utc;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::internal::chain_state::ChainState;
use crate::internal::partitioning::partition_for;
use crate::internal::schema_cache::SchemaCache;
use crate::models::{Event, ProducerMeta, ResetScope};
use crate::sdk::EventBrokerSdk;
use crate::typed_event::TypedEvent;

use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};

use super::backend::{IngestOutcome, ProducerBackend};
use super::sync_producer::gts_pattern_matches;
use super::{ProducerBuilder, ProducerMode, ValidationTiming};

/// Async producer backed by a modkit-db transactional outbox.
/// Obtained from [`ProducerBuilder::build_async`].
pub struct AsyncProducer {
    outbox: Arc<toolkit_db::outbox::Outbox>,
    queue_name: String,
    backend: Arc<dyn ProducerBackend>,
    schema_cache: Arc<SchemaCache>,
    chain_state: Arc<ChainState>,
    chain_mode: ProducerMode,
    /// Lazily minted; `None` until the outbox processor first delivers an event.
    producer_id: Arc<tokio::sync::Mutex<Option<ProducerId>>>,
    event_type_patterns: Vec<String>,
    resolved_type_ids: Arc<tokio::sync::RwLock<std::collections::HashSet<String>>>,
    validation: ValidationTiming,
    client_agent: String,
}

impl AsyncProducer {
    pub(crate) async fn build(
        ctx: &SecurityContext,
        builder: ProducerBuilder,
        db: toolkit_db::Db,
        queue_name: String,
    ) -> Result<Self, EventBrokerError> {
        let backend = builder.backend.ok_or_else(|| {
            EventBrokerError::Internal("ProducerBuilder: backend not wired".into())
        })?;
        let schema_cache = builder.schema_cache.ok_or_else(|| {
            EventBrokerError::Internal("ProducerBuilder: schema_cache not wired".into())
        })?;
        let chain_state = Arc::new(ChainState::new());

        // Eager validation (same as sync producer).
        let mut resolved_ids = std::collections::HashSet::new();
        if builder.validation_timing == ValidationTiming::Eager {
            let ids = schema_cache
                .resolve_all_patterns(ctx, &builder.event_type_patterns)
                .await?;
            resolved_ids.extend(ids);
        }

        // For async, producer_id minting is deferred to first delivery.
        // (builder.reuse handled in the processor closure).
        let reuse_id = builder.reuse;
        let producer_id = match (&builder.chain_mode, reuse_id) {
            (ProducerMode::Stateless, _) => None,
            (_, Some(id)) => {
                // Prime chain state from broker cursors on startup.
                let cursors = backend.get_producer_cursors(ctx, id).await?;
                let entries = cursors
                    .into_iter()
                    .map(|c| ((id, c.topic, c.partition), c.last_sequence));
                chain_state.bulk_prime(entries);
                Some(id)
            }
            _ => None, // Lazy mint on first delivery
        };

        let client_agent = builder
            .client_agent
            .clone()
            .unwrap_or_else(|| EventBrokerSdk::default_client_agent().to_owned());

        // Set up the modkit-db outbox with a leased processor that drains rows to ingest.
        let processor_backend = backend.clone();
        let handle = toolkit_db::outbox::Outbox::builder(db)
            .queue(&queue_name, toolkit_db::outbox::Partitions::of(16))
            .leased(IngestProcessor {
                backend: processor_backend,
            })
            .start()
            .await
            .map_err(|e| EventBrokerError::Internal(format!("outbox setup: {e}")))?;
        let outbox = handle.outbox().clone();

        Ok(Self {
            outbox,
            queue_name,
            backend,
            schema_cache,
            chain_state,
            chain_mode: builder.chain_mode,
            producer_id: Arc::new(tokio::sync::Mutex::new(producer_id)),
            event_type_patterns: builder.event_type_patterns,
            resolved_type_ids: Arc::new(tokio::sync::RwLock::new(resolved_ids)),
            validation: builder.validation_timing,
            client_agent,
        })
    }

    /// Enqueue an event into the modkit-db outbox inside `txn`.
    pub async fn publish<E, TX>(
        &self,
        ctx: &SecurityContext,
        txn: &TX,
        event: E,
    ) -> Result<(), EventBrokerError>
    where
        E: TypedEvent,
        TX: toolkit_db::secure::DBRunner + Sync + ?Sized,
    {
        let type_id = E::TYPE_ID;
        let topic = E::TOPIC;

        // Step 1: Declaration check.
        self.check_declared(type_id).await?;

        // Step 2+3: Schema validation (MUST NOT call types-registry inside txn for Lazy).
        let data = serde_json::to_value(&event)
            .map_err(|e| EventBrokerError::Internal(format!("serialize event data: {e}")))?;
        match self.validation {
            ValidationTiming::Eager => {
                self.schema_cache.validate(type_id, &data).await?;
            }
            ValidationTiming::Lazy => {
                if !self.schema_cache.is_cached(type_id).await {
                    return Err(EventBrokerError::SchemaNotPrepared {
                        type_id: type_id.to_owned(),
                        detail: "call `producer.prepare::<E>(&ctx)` outside the txn first".into(),
                        instance: String::new(),
                    });
                }
                self.schema_cache.validate(type_id, &data).await?;
            }
        }

        // Step 4: Compute partition.
        let subject_str = event.subject();
        let partition_key_str = event.partition_key();
        let key = partition_key_str.as_deref().unwrap_or(subject_str.as_ref());
        let partition = partition_for(key, 16); // default; see get_partition_count note

        // Step 5: Stamp ProducerMeta.
        let meta = self.build_meta(topic, partition).await;

        // Step 6: Build wire envelope.
        let tenant_id = event.tenant_id().unwrap_or_else(|| ctx.subject_tenant_id());
        let wire_event = Event {
            id: Uuid::now_v7(),
            type_id: type_id.to_owned(),
            topic: topic.to_owned(),
            tenant_id,
            source: E::SOURCE.to_owned(),
            subject: subject_str.into_owned(),
            subject_type: E::SUBJECT_TYPE.to_owned(),
            partition_key: partition_key_str.map(|s| s.into_owned()),
            occurred_at: Utc::now(),
            trace_parent: event.trace_parent().map(|s| s.into_owned()),
            data: Some(data),
            partition: None,
            sequence: None,
            sequence_time: None,
            offset: None,
            offset_time: None,
            meta,
        };

        // Step 7: Serialise via the outbox wire DTO (Event is a plain domain type).
        let payload = serde_json::to_vec(&OutboxEvent::from(&wire_event))
            .map_err(|e| EventBrokerError::Internal(format!("serialize wire event: {e}")))?;

        // Step 8: Enqueue inside caller's transaction (atomically with their business state).
        self.outbox
            .enqueue(
                txn,
                &self.queue_name,
                partition,
                payload,
                "application/json",
            )
            .await
            .map_err(|e| EventBrokerError::Internal(format!("outbox enqueue: {e}")))?;

        // Advance chain tracker at enqueue time (not at delivery - see design D7).
        if let Some(m) = &wire_event.meta {
            if let (Some(pid), Some(seq)) = (m.producer_id, m.sequence) {
                let pid = crate::ids::ProducerId(pid);
                self.chain_state
                    .advance((pid, topic.to_owned(), partition), seq);
            }
        }

        Ok(())
    }

    pub async fn prepare<E: TypedEvent>(
        &self,
        ctx: &SecurityContext,
    ) -> Result<(), EventBrokerError> {
        let type_id = E::TYPE_ID;
        self.check_declared(type_id).await?;
        if !self.schema_cache.is_cached(type_id).await {
            self.schema_cache.resolve_one(ctx, type_id).await?;
            self.resolved_type_ids
                .write()
                .await
                .insert(type_id.to_owned());
        }
        Ok(())
    }

    pub async fn prepare_all(&self, ctx: &SecurityContext) -> Result<(), EventBrokerError> {
        let ids = self
            .schema_cache
            .resolve_all_patterns(ctx, &self.event_type_patterns)
            .await?;
        self.resolved_type_ids.write().await.extend(ids);
        Ok(())
    }

    pub fn producer_id(&self) -> Option<ProducerId> {
        self.producer_id.try_lock().ok().and_then(|g| *g)
    }

    pub async fn reset_chain(
        &self,
        ctx: &SecurityContext,
        scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        let pid = self.producer_id().ok_or_else(|| {
            EventBrokerError::Internal("reset_chain: producer_id not yet minted".into())
        })?;
        self.backend.reset_producer_chain(ctx, pid, scope).await
    }

    // ---- Private helpers ----

    async fn check_declared(&self, type_id: &str) -> Result<(), EventBrokerError> {
        if self.resolved_type_ids.read().await.contains(type_id) {
            return Ok(());
        }
        let matches = self
            .event_type_patterns
            .iter()
            .any(|p| gts_pattern_matches(p, type_id));
        if !matches {
            return Err(EventBrokerError::EventTypeNotDeclared {
                type_id: type_id.to_owned(),
                detail: "this event type does not match any declared event_type_patterns".into(),
                instance: String::new(),
            });
        }
        Ok(())
    }

    async fn build_meta(&self, topic: &str, partition: u32) -> Option<ProducerMeta> {
        match self.chain_mode {
            ProducerMode::Stateless => None,
            ProducerMode::Monotonic => {
                let pid_guard = self.producer_id.lock().await;
                pid_guard.map(|pid| {
                    let key = (pid, topic.to_owned(), partition);
                    let last = self.chain_state.peek(&key);
                    ProducerMeta {
                        version: 1,
                        producer_id: Some(pid.0),
                        previous: None,
                        sequence: Some(last + 1),
                        partition_hint: Some(partition),
                    }
                })
            }
            ProducerMode::Chained => {
                let pid_guard = self.producer_id.lock().await;
                pid_guard.map(|pid| {
                    let key = (pid, topic.to_owned(), partition);
                    let last = self.chain_state.peek(&key);
                    ProducerMeta {
                        version: 1,
                        producer_id: Some(pid.0),
                        previous: Some(last),
                        sequence: Some(last + 1),
                        partition_hint: Some(partition),
                    }
                })
            }
        }
    }
}

// ---- Outbox wire payload ----

/// Serializable mirror of [`Event`] used solely as the modkit-db outbox queue
/// payload. `Event` itself is a plain domain type (no serde); this DTO owns the
/// wire contract for the enqueue/drain round-trip. The shape matches
/// `event.v1.schema.json` (same field renames + `skip_serializing_if`).
#[derive(serde::Serialize, serde::Deserialize)]
struct OutboxEvent {
    id: uuid::Uuid,
    #[serde(rename = "type")]
    type_id: String,
    topic: String,
    tenant_id: uuid::Uuid,
    source: String,
    subject: String,
    subject_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition_key: Option<String>,
    occurred_at: chrono::DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sequence_time: Option<chrono::DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    offset_time: Option<chrono::DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    meta: Option<OutboxProducerMeta>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OutboxProducerMeta {
    version: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    producer_id: Option<uuid::Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sequence: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition_hint: Option<u32>,
}

impl From<&Event> for OutboxEvent {
    fn from(e: &Event) -> Self {
        Self {
            id: e.id,
            type_id: e.type_id.clone(),
            topic: e.topic.clone(),
            tenant_id: e.tenant_id,
            source: e.source.clone(),
            subject: e.subject.clone(),
            subject_type: e.subject_type.clone(),
            partition_key: e.partition_key.clone(),
            occurred_at: e.occurred_at,
            trace_parent: e.trace_parent.clone(),
            data: e.data.clone(),
            partition: e.partition,
            sequence: e.sequence,
            sequence_time: e.sequence_time,
            offset: e.offset,
            offset_time: e.offset_time,
            meta: e.meta.as_ref().map(|m| OutboxProducerMeta {
                version: m.version,
                producer_id: m.producer_id,
                previous: m.previous,
                sequence: m.sequence,
                partition_hint: m.partition_hint,
            }),
        }
    }
}

impl From<OutboxEvent> for Event {
    fn from(d: OutboxEvent) -> Self {
        Self {
            id: d.id,
            type_id: d.type_id,
            topic: d.topic,
            tenant_id: d.tenant_id,
            source: d.source,
            subject: d.subject,
            subject_type: d.subject_type,
            partition_key: d.partition_key,
            occurred_at: d.occurred_at,
            trace_parent: d.trace_parent,
            data: d.data,
            partition: d.partition,
            sequence: d.sequence,
            sequence_time: d.sequence_time,
            offset: d.offset,
            offset_time: d.offset_time,
            meta: d.meta.map(|m| ProducerMeta {
                version: m.version,
                producer_id: m.producer_id,
                previous: m.previous,
                sequence: m.sequence,
                partition_hint: m.partition_hint,
            }),
        }
    }
}

// ---- Outbox processor ----

/// Leased outbox processor: deserialises each payload, calls the broker ingest API,
/// maps responses to outbox HandlerResult semantics.
struct IngestProcessor {
    backend: Arc<dyn ProducerBackend>,
}

#[async_trait::async_trait]
impl LeasedMessageHandler for IngestProcessor {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        let event: Event = match serde_json::from_slice::<OutboxEvent>(&msg.payload) {
            Ok(dto) => dto.into(),
            Err(e) => return MessageResult::Reject(format!("decode wire event: {e}")),
        };
        // Use anonymous system context; the backend handles auth at the transport layer.
        let ctx = SecurityContext::anonymous();
        match self.backend.ingest_event(&ctx, &event).await {
            Ok(IngestOutcome::Accepted)
            | Ok(IngestOutcome::Persisted)
            | Ok(IngestOutcome::Duplicate) => MessageResult::Ok,
            Err(crate::error::EventBrokerError::Transport(_))
            | Err(crate::error::EventBrokerError::RateLimitExceeded { .. }) => MessageResult::Retry,
            Err(e) => MessageResult::Reject(e.to_string()),
        }
    }
}
