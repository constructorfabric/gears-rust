use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gts::GtsTypeId;
use toolkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};

use crate::api::{Ack, IngestOutcome, ProducerMode};
use crate::error::{EventBrokerError, json_error_category};
use crate::gts::{EventMeta, EventV1};
use crate::ids::ProducerId;

use super::db::DbProducer;

/// The `ProducerOutboxEnvelope` shape version written on every producer-outbox
/// row; the drain rejects an envelope of any other version right after it
/// decodes, before any broker call.
pub(crate) const PRODUCER_OUTBOX_ENVELOPE_VERSION: u16 = 1;
/// The outbox payload type the SDK writes on every producer-outbox row. No
/// outbox claim filters on it: the processor decodes every row of its queue as
/// a `ProducerOutboxEnvelope`.
pub(crate) const PRODUCER_OUTBOX_PAYLOAD_TYPE: &str =
    "application/vnd.constructorfabric.event-broker.producer-outbox+json;version=1";

/// A producer's chain is kept per `(producer_id, topic)` (ADR-0004 "Chain Scope").
type ProducerOutboxCursorKey = (ProducerId, String);
type ProducerOutboxCursorMap = HashMap<ProducerOutboxCursorKey, i64>;
type SharedProducerOutboxCursor = Arc<Mutex<ProducerOutboxCursorMap>>;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProducerOutboxEnvelope {
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
pub(crate) struct ProducerOutboxDiagnostics {
    sdk_client_agent: String,
}

impl ProducerOutboxEnvelope {
    /// Wraps `event` with its routing and producer metadata for the outbox.
    ///
    /// # Errors
    ///
    /// [`EventBrokerError::Internal`] when `event` has no source, a broken internal
    /// invariant: a prepared event always carries the producer's source.
    pub(crate) fn from_event(
        event: EventV1,
        topic: String,
        producer_mode: ProducerMode,
        sdk_client_agent: String,
    ) -> Result<Self, EventBrokerError> {
        let Some(source) = event.source().map(str::to_owned) else {
            return Err(EventBrokerError::Internal(
                "a prepared event has a source".to_owned(),
            ));
        };
        Ok(Self {
            version: PRODUCER_OUTBOX_ENVELOPE_VERSION,
            event_id: event.id(),
            event_type_id: event.event_type().clone(),
            topic,
            tenant_id: event.tenant_id(),
            source,
            subject: event.subject().to_owned(),
            subject_type: event.subject_type().clone(),
            occurred_at: event.occurred_at(),
            trace_parent: event.trace_parent().map(str::to_owned),
            data: event.into_data(),
            producer_mode,
            diagnostic_metadata: ProducerOutboxDiagnostics { sdk_client_agent },
        })
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
    ) -> Result<EventV1, EventBrokerError> {
        let meta = match (self.producer_mode, producer_id) {
            (ProducerMode::Stateless, _) => None,
            (_, None) => {
                return Err(EventBrokerError::Internal(
                    "producer outbox message of a deduplicating producer has no producer id"
                        .to_owned(),
                ));
            }
            (ProducerMode::Monotonic, Some(producer_id)) => Some(EventMeta {
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
                Some(EventMeta {
                    version: 1,
                    producer_id: Some(producer_id.0),
                    previous: Some(previous),
                    sequence: Some(seq),
                })
            }
        };
        let mut builder = match &self.data {
            Some(data) => EventV1::builder_for(self.event_type_id.clone(), data.clone()),
            None => EventV1::builder_without_data(self.event_type_id.clone()),
        }
        .id(self.event_id)
        .tenant_id(self.tenant_id)
        .source(self.source.clone())
        .subject(self.subject.clone())
        .subject_type_id(self.subject_type.clone())
        .occurred_at(self.occurred_at);
        if let Some(trace_parent) = &self.trace_parent {
            builder = builder.trace_parent(trace_parent.clone());
        }
        let mut event = builder.build()?;
        if let Some(meta) = meta {
            event.stamp_meta(meta);
        }
        Ok(event)
    }
}

/// The queue name the `OutboxSetup::process_outbox_payload*_for_test` hooks
/// drain as.
#[cfg(any(test, feature = "test-util"))]
#[doc(hidden)]
pub const TEST_QUEUE: &str = "event-broker-producer-test";

pub(super) struct ProducerOutboxProcessor {
    producer: DbProducer,
    queue: String,
    cursor_state: SharedProducerOutboxCursor,
}

/// How a failed drain step (a broker call, the partition's producer id, the
/// event build) is answered: a transient failure retries, anything else
/// rejects the message with its variant name only, never its `Display`,
/// which can quote payload values.
pub(super) fn retry_or_reject(err: &EventBrokerError) -> MessageResult {
    match err {
        EventBrokerError::Transport(_)
        | EventBrokerError::RateLimitExceeded { .. }
        | EventBrokerError::RateLimited { .. } => MessageResult::Retry,
        err => MessageResult::Reject(format!("producer outbox message rejected: {}", err.kind())),
    }
}

impl ProducerOutboxProcessor {
    /// The processor that drains `queue` through `producer`, with no chain
    /// cursor read yet.
    pub(super) fn new(producer: DbProducer, queue: String) -> Self {
        Self {
            producer,
            queue,
            cursor_state: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Seeds the chain cursor of `payload`'s topic to `previous` for the
    /// producer id of partition 0 of [`TEST_QUEUE`], as a drain that starts
    /// mid-chain. A payload that does not decode, or a partition whose id
    /// does not resolve, seeds nothing.
    #[cfg(any(test, feature = "test-util"))]
    pub(super) async fn seed_test_cursor(&self, payload: &[u8], previous: i64) {
        if let Ok(envelope) = serde_json::from_slice::<ProducerOutboxEnvelope>(payload)
            && let Ok(producer_id) = self.producer.partition_producer(TEST_QUEUE, 0).await
        {
            self.cursor_state
                .lock()
                .expect("producer outbox cursor state lock poisoned")
                .insert((producer_id, envelope.topic), previous);
        }
    }

    /// Drains `payload` as message `seq` of partition id 0.
    #[cfg(any(test, feature = "test-util"))]
    pub(super) async fn handle_test_payload(&self, payload: Vec<u8>, seq: i64) -> MessageResult {
        let msg = OutboxMessage {
            partition_id: 0,
            seq,
            payload,
            payload_type: PRODUCER_OUTBOX_PAYLOAD_TYPE.to_owned(),
            created_at: chrono::Utc::now(),
            attempts: 0,
        };
        self.handle(&msg).await
    }

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
    ) -> Result<EventV1, EventBrokerError> {
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
            Err(err) => {
                return MessageResult::Reject(format!(
                    "producer outbox message rejected: {}",
                    err.kind()
                ));
            }
        };
        match self.publish(&envelope.topic, &event).await {
            Err(EventBrokerError::SequenceViolation { .. }) => MessageResult::Reject(
                "producer outbox chained sequence divergence persisted after cursor refresh"
                    .to_owned(),
            ),
            result => self.answer(result, partition).await,
        }
    }

    /// Publishes `event` and records its sequence as the chain's last for
    /// `topic`. Every outcome means the broker holds the event: a `Duplicate`
    /// says it holds this sequence or a later one, so the next message chains
    /// onto it instead of paying a violation and a cursor refresh. The cursor
    /// only ever moves forward.
    async fn publish(
        &self,
        topic: &str,
        event: &EventV1,
    ) -> Result<IngestOutcome, EventBrokerError> {
        let outcome = self
            .producer
            .broker
            .publish(&self.producer.ctx, event, Ack::Accepted)
            .await?;
        // Irrefutable, so a new outcome has to decide whether it advances.
        let (IngestOutcome::Accepted | IngestOutcome::Persisted | IngestOutcome::Duplicate) =
            outcome;
        if let Some(meta) = event.meta()
            && let (Some(producer_id), Some(sequence)) = (meta.producer_id, meta.sequence)
        {
            let mut cursor_state = self
                .cursor_state
                .lock()
                .expect("producer outbox cursor state lock poisoned");
            let last = cursor_state
                .entry((ProducerId(producer_id), topic.to_owned()))
                .or_insert(sequence);
            *last = (*last).max(sequence);
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
            Err(err) => {
                // Serde's message quotes the payload, so only its category is
                // logged and stored.
                let category = json_error_category(&err);
                tracing::error!(
                    queue = self.queue.as_str(),
                    partition_id = msg.partition_id,
                    seq = msg.seq,
                    payload_type = msg.payload_type.as_str(),
                    reason = "undecodable",
                    category,
                    "producer outbox message rejected"
                );
                return MessageResult::Reject(format!(
                    "undecodable producer envelope ({category})"
                ));
            }
        };
        if envelope.version != PRODUCER_OUTBOX_ENVELOPE_VERSION {
            tracing::error!(
                queue = self.queue.as_str(),
                partition_id = msg.partition_id,
                seq = msg.seq,
                payload_type = msg.payload_type.as_str(),
                reason = "version",
                version = envelope.version,
                "producer outbox message rejected"
            );
            return MessageResult::Reject(format!(
                "unsupported producer envelope version {}",
                envelope.version
            ));
        }
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
