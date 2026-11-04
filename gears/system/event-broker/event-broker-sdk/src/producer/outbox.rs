use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gts::GtsTypeId;
use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};

use crate::api::{IngestOutcome, ProducerMode};
use crate::error::EventBrokerError;
use crate::ids::ProducerId;
use crate::models::{Event, ProducerMeta};

use super::db::DbProducer;

pub const PRODUCER_OUTBOX_ENVELOPE_VERSION: u16 = 2;
pub const PRODUCER_OUTBOX_PAYLOAD_TYPE: &str =
    "application/vnd.constructorfabric.event-broker.producer-outbox+json;version=2";

/// A producer's chain is kept per `(producer_id, topic)` (ADR-0004 "Chain Scope").
type ProducerOutboxCursorKey = (ProducerId, String);
type ProducerOutboxCursorMap = HashMap<ProducerOutboxCursorKey, i64>;
type SharedProducerOutboxCursor = Arc<Mutex<ProducerOutboxCursorMap>>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProducerOutboxEnvelope {
    version: u16,
    event_id: uuid::Uuid,
    #[serde(rename = "type")]
    event_type_id: GtsTypeId,
    topic: String,
    tenant_id: uuid::Uuid,
    source: String,
    subject: String,
    subject_type: GtsTypeId,
    occurred_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_parent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    // Mapped through `producer_mode_wire`: SDK types carry no serde derives.
    #[serde(with = "producer_mode_wire")]
    producer_mode: ProducerMode,
    diagnostic_metadata: ProducerOutboxDiagnostics,
}

/// Private wire mirror of [`ProducerMode`] inside [`ProducerOutboxEnvelope`].
/// `ProducerMode` carries no serde derives (SDK types stay transport-free), so
/// this local serde-derived mirror is the one place the mode's wire form is
/// defined; `#[serde(with = "producer_mode_wire")]` maps the envelope field
/// through it. It is an outbox row format, not a REST DTO: it has no OpenAPI
/// schema and does not belong under `api/rest`.
mod producer_mode_wire {
    use serde::{Deserialize, Serialize};

    use crate::api::ProducerMode;

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum ProducerModeWire {
        Stateless,
        Monotonic,
        Chained,
    }

    impl From<ProducerMode> for ProducerModeWire {
        fn from(mode: ProducerMode) -> Self {
            match mode {
                ProducerMode::Stateless => Self::Stateless,
                ProducerMode::Monotonic => Self::Monotonic,
                ProducerMode::Chained => Self::Chained,
            }
        }
    }

    impl From<ProducerModeWire> for ProducerMode {
        fn from(wire: ProducerModeWire) -> Self {
            match wire {
                ProducerModeWire::Stateless => Self::Stateless,
                ProducerModeWire::Monotonic => Self::Monotonic,
                ProducerModeWire::Chained => Self::Chained,
            }
        }
    }

    pub(super) fn serialize<S: serde::Serializer>(
        mode: &ProducerMode,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        ProducerModeWire::from(*mode).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<ProducerMode, D::Error> {
        Ok(ProducerModeWire::deserialize(deserializer)?.into())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProducerOutboxDiagnostics {
    sdk_client_agent: String,
}

impl ProducerOutboxEnvelope {
    /// Wraps `event` with its routing and producer metadata for the outbox.
    pub(crate) fn from_event(
        event: Event,
        topic: String,
        producer_mode: ProducerMode,
        sdk_client_agent: String,
    ) -> Self {
        Self {
            version: PRODUCER_OUTBOX_ENVELOPE_VERSION,
            event_id: event.id,
            event_type_id: event.type_id,
            topic,
            tenant_id: event.tenant_id,
            source: event.source,
            subject: event.subject,
            subject_type: event.subject_type,
            occurred_at: event.occurred_at,
            trace_parent: event.trace_parent,
            data: event.data,
            producer_mode,
            diagnostic_metadata: ProducerOutboxDiagnostics { sdk_client_agent },
        }
    }

    /// The event this envelope publishes as message `seq` under
    /// `producer_id`, the id of the outbox partition it drains from.
    /// `chained_previous` is the chain's last accepted sequence, for a chained
    /// producer only.
    fn to_event(
        &self,
        seq: i64,
        producer_id: Option<ProducerId>,
        chained_previous: Option<i64>,
    ) -> Result<Event, EventBrokerError> {
        if self.version != PRODUCER_OUTBOX_ENVELOPE_VERSION {
            return Err(EventBrokerError::Internal(format!(
                "unsupported producer outbox envelope version {}",
                self.version
            )));
        }
        let meta = match (self.producer_mode, producer_id) {
            (ProducerMode::Stateless, _) => None,
            (_, None) => {
                return Err(EventBrokerError::Internal(
                    "producer outbox message of a deduplicating producer has no producer id"
                        .to_owned(),
                ));
            }
            (ProducerMode::Monotonic, Some(producer_id)) => Some(ProducerMeta {
                version: 1,
                producer_id: Some(producer_id.0),
                previous: None,
                sequence: Some(seq),
            }),
            (ProducerMode::Chained, Some(producer_id)) => {
                let previous = chained_previous.ok_or_else(|| {
                    EventBrokerError::Internal(
                        "producer outbox chained publish is missing previous sequence".to_owned(),
                    )
                })?;
                Some(ProducerMeta {
                    version: 1,
                    producer_id: Some(producer_id.0),
                    previous: Some(previous),
                    sequence: Some(seq),
                })
            }
        };
        Ok(Event {
            id: self.event_id,
            type_id: self.event_type_id.clone(),
            tenant_id: self.tenant_id,
            source: self.source.clone(),
            subject: self.subject.clone(),
            subject_type: self.subject_type.clone(),
            occurred_at: self.occurred_at,
            trace_parent: self.trace_parent.clone(),
            data: self.data.clone(),
            partition: None,
            sequence: None,
            sequence_time: None,
            meta,
        })
    }
}

#[derive(Clone)]
pub struct ProducerOutbox {
    producer: DbProducer,
    outbox: Arc<toolkit_db::outbox::Outbox>,
    queue: String,
    partitions: u32,
}

impl ProducerOutbox {
    /// Enqueue one typed event within the caller's transaction.
    ///
    /// The producer outbox row is written atomically with `runner`'s
    /// transaction, but the write does **not** wake the sequencer on its own.
    /// Call [`Wake::fire`](toolkit_db::outbox::Wake::fire) on
    /// the returned handle once that transaction has committed; on rollback,
    /// drop the handle instead. Until it is fired the event is durable but
    /// stays unpublished until the outbox's cold reconciler discovers it.
    ///
    /// # Errors
    ///
    /// Returns an error if the event fails producer validation, the envelope
    /// cannot be serialized, or the database rejects the write.
    pub async fn enqueue<E: crate::typed_event::TypedEvent>(
        &self,
        runner: &(impl toolkit_db::secure::DBRunner + Sync + ?Sized),
        event: E,
    ) -> Result<toolkit_db::outbox::Wake, EventBrokerError> {
        let (partition, envelope) = self
            .producer
            .outbox_envelope(event, self.partitions)
            .await?;
        let payload = serde_json::to_vec(&envelope).map_err(|err| {
            EventBrokerError::Internal(format!("serialize producer outbox envelope: {err}"))
        })?;
        let record = toolkit_db::outbox::Record::to(&self.queue, partition)
            .payload(payload, PRODUCER_OUTBOX_PAYLOAD_TYPE)
            .build()
            .map_err(|err| EventBrokerError::Internal(format!("producer outbox enqueue: {err}")))?;
        self.outbox
            .enqueue(runner, record)
            .await
            .map_err(|err| EventBrokerError::Internal(format!("producer outbox enqueue: {err}")))
    }

    /// Enqueue several typed events for this producer in one batch.
    ///
    /// The whole batch is written atomically with `runner`'s transaction and,
    /// like [`enqueue`](Self::enqueue), does not wake the sequencer on its own.
    /// Call [`Wake::fire`](toolkit_db::outbox::Wake::fire) on
    /// the returned handle once that transaction has committed; on rollback,
    /// drop the handle instead.
    ///
    /// # Errors
    ///
    /// Returns an error if any event fails producer validation, an envelope
    /// cannot be serialized, or the database rejects the write. A batch is
    /// all-or-nothing: one rejected event writes none.
    pub async fn enqueue_batch<E: crate::typed_event::TypedEvent>(
        &self,
        runner: &(impl toolkit_db::secure::DBRunner + Sync + ?Sized),
        events: impl IntoIterator<Item = E>,
    ) -> Result<toolkit_db::outbox::Wake, EventBrokerError> {
        // Every event in the batch shares the queue and the payload type, so
        // the batch states them once. No trace is attached: a produced event
        // is already identified by its own id and `trace_parent`, and giving
        // each one a trace row would be a record nobody reads.
        let mut batch =
            toolkit_db::outbox::Records::to(&self.queue).payload_type(PRODUCER_OUTBOX_PAYLOAD_TYPE);
        for event in events {
            let (partition, envelope) = self
                .producer
                .outbox_envelope(event, self.partitions)
                .await?;
            let payload = serde_json::to_vec(&envelope).map_err(|err| {
                EventBrokerError::Internal(format!("serialize producer outbox envelope: {err}"))
            })?;
            batch = batch.push(partition, payload);
        }
        let batch = batch.build().map_err(|err| {
            EventBrokerError::Internal(format!("producer outbox batch enqueue: {err}"))
        })?;
        self.outbox
            .enqueue_batch(runner, batch)
            .await
            .map_err(|err| {
                EventBrokerError::Internal(format!("producer outbox batch enqueue: {err}"))
            })
    }
}

#[derive(Clone)]
pub struct ProducerOutboxQueue {
    producer: DbProducer,
    queue: String,
    partitions: toolkit_db::outbox::Partitions,
}

impl ProducerOutboxQueue {
    /// Creates a queue over `producer`; fails on a blank queue name.
    pub(crate) fn new(
        producer: DbProducer,
        queue: String,
        partitions: toolkit_db::outbox::Partitions,
    ) -> Result<Self, EventBrokerError> {
        if queue.trim().is_empty() {
            return Err(EventBrokerError::InvalidProducerOptions {
                detail: "producer outbox queue name must not be empty".to_owned(),
            });
        }
        Ok(Self {
            producer,
            queue,
            partitions,
        })
    }

    pub fn register(
        &self,
        builder: toolkit_db::outbox::OutboxBuilder,
    ) -> toolkit_db::outbox::LeasedQueueBuilder {
        builder
            .queue(&self.queue, self.partitions)
            .leased(ProducerOutboxProcessor {
                producer: self.producer.clone(),
                queue: self.queue.clone(),
                cursor_state: Arc::new(Mutex::new(HashMap::new())),
            })
    }

    pub fn bind(&self, handle: &toolkit_db::outbox::OutboxHandle) -> ProducerOutbox {
        ProducerOutbox {
            producer: self.producer.clone(),
            outbox: Arc::clone(handle.outbox()),
            queue: self.queue.clone(),
            partitions: u32::from(self.partitions.count()),
        }
    }

    pub async fn start(
        &self,
        builder: toolkit_db::outbox::OutboxBuilder,
    ) -> Result<ProducerOutboxHandle, EventBrokerError> {
        let handle =
            self.register(builder).start().await.map_err(|err| {
                EventBrokerError::Internal(format!("start producer outbox: {err}"))
            })?;
        let outbox = self.bind(&handle);
        Ok(ProducerOutboxHandle { handle, outbox })
    }
}

pub struct ProducerOutboxHandle {
    handle: toolkit_db::outbox::OutboxHandle,
    outbox: ProducerOutbox,
}

impl ProducerOutboxHandle {
    pub fn outbox(&self) -> &ProducerOutbox {
        &self.outbox
    }

    pub async fn stop(self) {
        self.handle.stop().await;
    }
}

/// The queue name the `process_outbox_payload*_for_test` helpers drain as.
#[doc(hidden)]
pub const TEST_QUEUE: &str = "event-broker-producer-test";

impl DbProducer {
    /// The producer id messages of `partition` of `queue` publish under,
    /// registering it now if the partition has none yet - as its first drain
    /// would.
    #[doc(hidden)]
    pub async fn partition_producer_for_test(
        &self,
        queue: &str,
        partition: i64,
    ) -> Result<ProducerId, EventBrokerError> {
        self.partition_producer(queue, partition).await
    }

    /// Every stored partition registration of this producer's key, as
    /// `(queue, partition, producer id, generation)`, ordered by queue and
    /// partition.
    #[doc(hidden)]
    pub async fn registrations_for_test(
        &self,
    ) -> Result<Vec<(String, i64, ProducerId, i64)>, EventBrokerError> {
        self.stored_partition_registrations().await
    }

    /// Drains one message of partition id 0 of [`TEST_QUEUE`] through the
    /// producer outbox handler.
    #[doc(hidden)]
    pub async fn process_outbox_payload_for_test(
        &self,
        payload: Vec<u8>,
        seq: i64,
    ) -> MessageResult {
        self.process_outbox_payload_with_cursor_for_test(payload, seq, None)
            .await
    }

    /// As [`process_outbox_payload_for_test`](Self::process_outbox_payload_for_test),
    /// with the chained cursor of the envelope's topic seeded to
    /// `initial_chained_previous` for partition 0's producer id.
    #[doc(hidden)]
    pub async fn process_outbox_payload_with_cursor_for_test(
        &self,
        payload: Vec<u8>,
        seq: i64,
        initial_chained_previous: Option<i64>,
    ) -> MessageResult {
        let cursor_state = Arc::new(Mutex::new(HashMap::new()));
        if let Some(previous) = initial_chained_previous
            && let Ok(envelope) = serde_json::from_slice::<ProducerOutboxEnvelope>(&payload)
            && let Ok(producer_id) = self.partition_producer(TEST_QUEUE, 0).await
        {
            cursor_state
                .lock()
                .expect("producer outbox cursor state lock poisoned")
                .insert((producer_id, envelope.topic), previous);
        }
        let processor = ProducerOutboxProcessor {
            producer: self.clone(),
            queue: TEST_QUEUE.to_owned(),
            cursor_state,
        };
        let msg = OutboxMessage {
            partition_id: 0,
            seq,
            payload,
            payload_type: PRODUCER_OUTBOX_PAYLOAD_TYPE.to_owned(),
            created_at: chrono::Utc::now(),
            attempts: 0,
        };
        processor.handle(&msg).await
    }
}

struct ProducerOutboxProcessor {
    producer: DbProducer,
    queue: String,
    cursor_state: SharedProducerOutboxCursor,
}

/// How a failed broker call is answered: a transient failure retries, anything
/// else rejects the message.
fn retry_or_reject(err: &EventBrokerError) -> MessageResult {
    match err {
        EventBrokerError::Transport(_)
        | EventBrokerError::RateLimitExceeded { .. }
        | EventBrokerError::RateLimited { .. } => MessageResult::Retry,
        err => MessageResult::Reject(err.to_string()),
    }
}

impl ProducerOutboxProcessor {
    /// The producer id messages of `partition` publish under, or `None` for a
    /// stateless producer.
    async fn producer_for(
        &self,
        envelope: &ProducerOutboxEnvelope,
        partition: i64,
    ) -> Result<Option<ProducerId>, EventBrokerError> {
        if envelope.producer_mode == ProducerMode::Stateless {
            return Ok(None);
        }
        self.producer
            .partition_producer(&self.queue, partition)
            .await
            .map(Some)
    }

    async fn event_for_message(
        &self,
        envelope: &ProducerOutboxEnvelope,
        producer_id: Option<ProducerId>,
        seq: i64,
    ) -> Result<Event, EventBrokerError> {
        match (envelope.producer_mode, producer_id) {
            (ProducerMode::Chained, Some(producer_id)) => {
                let previous = self
                    .cursor_for((producer_id, envelope.topic.clone()))
                    .await?;
                envelope.to_event(seq, Some(producer_id), Some(previous))
            }
            _ => envelope.to_event(seq, producer_id, None),
        }
    }

    async fn cursor_for(&self, key: ProducerOutboxCursorKey) -> Result<i64, EventBrokerError> {
        if let Some(previous) = self
            .cursor_state
            .lock()
            .expect("producer outbox cursor state lock poisoned")
            .get(&key)
            .copied()
        {
            return Ok(previous);
        }
        self.refresh_cursor(key).await
    }

    async fn refresh_cursor(&self, key: ProducerOutboxCursorKey) -> Result<i64, EventBrokerError> {
        let (producer_id, topic) = key.clone();
        let cursors = self
            .producer
            .broker
            .get_producer_cursors(&self.producer.ctx, producer_id)
            .await?;
        let last_sequence = cursors
            .topics
            .into_iter()
            .find(|t| t.topic == topic)
            .map_or(-1, |t| t.last_sequence);
        self.cursor_state
            .lock()
            .expect("producer outbox cursor state lock poisoned")
            .insert(key, last_sequence);
        Ok(last_sequence)
    }

    /// The broker forgot `producer_id`: the partition gets a new one, and the
    /// message is retried under it. The broker's chain state for the old id is
    /// already gone, so nothing is kept by stalling.
    async fn replace_unknown(&self, partition: i64, producer_id: ProducerId) -> MessageResult {
        match self
            .producer
            .replace_unknown_producer(&self.queue, partition, producer_id)
            .await
        {
            Ok(()) => MessageResult::Retry,
            Err(err) => retry_or_reject(&err),
        }
    }

    async fn handle_sequence_violation(
        &self,
        envelope: &ProducerOutboxEnvelope,
        producer_id: ProducerId,
        partition: i64,
        seq: i64,
    ) -> MessageResult {
        let previous = match self
            .refresh_cursor((producer_id, envelope.topic.clone()))
            .await
        {
            Ok(previous) => previous,
            Err(EventBrokerError::UnknownProducer { producer_id, .. }) => {
                return self.replace_unknown(partition, producer_id).await;
            }
            Err(err) => return retry_or_reject(&err),
        };
        if seq <= previous {
            return MessageResult::Ok;
        }
        let event = match envelope.to_event(seq, Some(producer_id), Some(previous)) {
            Ok(event) => event,
            Err(err) => return MessageResult::Reject(err.to_string()),
        };
        match self.publish(&envelope.topic, &event).await {
            Err(EventBrokerError::SequenceViolation { .. }) => MessageResult::Reject(
                "producer outbox chained sequence divergence persisted after cursor refresh"
                    .to_owned(),
            ),
            result => self.answer(result, partition).await,
        }
    }

    /// Publishes `event` and, once accepted, records its sequence as the
    /// chain's last for `topic`.
    async fn publish(&self, topic: &str, event: &Event) -> Result<IngestOutcome, EventBrokerError> {
        let outcome = self
            .producer
            .broker
            .publish(&self.producer.ctx, event)
            .await?;
        if matches!(outcome, IngestOutcome::Accepted | IngestOutcome::Persisted)
            && let Some(meta) = &event.meta
            && let (Some(producer_id), Some(sequence)) = (meta.producer_id, meta.sequence)
        {
            self.cursor_state
                .lock()
                .expect("producer outbox cursor state lock poisoned")
                .insert((ProducerId(producer_id), topic.to_owned()), sequence);
        }
        Ok(outcome)
    }

    async fn answer(
        &self,
        result: Result<IngestOutcome, EventBrokerError>,
        partition: i64,
    ) -> MessageResult {
        match result {
            Ok(_) => MessageResult::Ok,
            Err(EventBrokerError::UnknownProducer { producer_id, .. }) => {
                self.replace_unknown(partition, producer_id).await
            }
            Err(err) => retry_or_reject(&err),
        }
    }
}

#[async_trait::async_trait]
impl LeasedMessageHandler for ProducerOutboxProcessor {
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        let envelope = match serde_json::from_slice::<ProducerOutboxEnvelope>(&msg.payload) {
            Ok(envelope) => envelope,
            Err(err) => return MessageResult::Reject(format!("decode producer envelope: {err}")),
        };
        let partition = msg.partition_id;
        // Resolving the partition's id and building the event can both call
        // the broker (registration, a chained cursor refresh), so they get the
        // same recovery as the publish: a forgotten id surfaces as
        // `UnknownProducer` at whichever call observes it first.
        let producer_id = match self.producer_for(&envelope, partition).await {
            Ok(producer_id) => producer_id,
            Err(err) => return retry_or_reject(&err),
        };
        let event = match self
            .event_for_message(&envelope, producer_id, msg.seq)
            .await
        {
            Ok(event) => event,
            Err(EventBrokerError::UnknownProducer { producer_id, .. }) => {
                return self.replace_unknown(partition, producer_id).await;
            }
            Err(err) => return retry_or_reject(&err),
        };
        match self.publish(&envelope.topic, &event).await {
            Err(EventBrokerError::SequenceViolation { .. })
                if envelope.producer_mode == ProducerMode::Chained =>
            {
                let producer_id = producer_id.expect("a chained producer has a producer id");
                self.handle_sequence_violation(&envelope, producer_id, partition, msg.seq)
                    .await
            }
            result => self.answer(result, partition).await,
        }
    }
}
