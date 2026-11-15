use std::sync::Arc;

use toolkit_canonical_errors::context::FieldViolation;
use toolkit_canonical_errors::resource_error;
use toolkit_canonical_errors::{CanonicalError, Http};

use crate::backend::BackendError;
use crate::ids::{ProducerId, SubscriptionId};
use crate::sequence::Sequence;

#[resource_error(gts_id!("cf.core.events.producer_options.v1~"))]
struct ProducerOptionsResourceError;
#[resource_error(gts_id!("cf.core.events.consumer_options.v1~"))]
struct ConsumerOptionsResourceError;
#[resource_error(gts_id!("cf.core.events.topic.v1~"))]
struct TopicResourceError;
#[resource_error(gts_id!("cf.core.events.consumer_group.v1~"))]
struct ConsumerGroupResourceError;
#[resource_error(gts_id!("cf.core.events.subscription.v1~"))]
struct SubscriptionResourceError;
#[resource_error(gts_id!("cf.core.events.producer.v1~"))]
struct ProducerResourceError;
#[resource_error(gts_id!("cf.core.events.partition.v1~"))]
struct PartitionResourceError;
#[resource_error(gts_id!("cf.core.events.event.v1~"))]
struct EventResourceError;
#[resource_error(gts_id!("cf.core.events.event_type.v1~"))]
struct EventTypeResourceError;
#[resource_error(gts_id!("cf.core.events.stream.v1~"))]
struct StreamResourceError;
#[resource_error(gts_id!("cf.core.events.storage.v1~"))]
struct StorageResourceError;
#[resource_error(gts_id!("cf.core.events.offset.v1~"))]
struct OffsetResourceError;
#[resource_error(gts_id!("cf.core.events.request.v1~"))]
struct RequestResourceError;

pub mod resources {
    use toolkit_gts::gts_id;

    pub const PRODUCER_OPTIONS: &str = gts_id!("cf.core.events.producer_options.v1~");
    pub const CONSUMER_OPTIONS: &str = gts_id!("cf.core.events.consumer_options.v1~");
    pub const TOPIC: &str = gts_id!("cf.core.events.topic.v1~");
    pub const CONSUMER_GROUP: &str = gts_id!("cf.core.events.consumer_group.v1~");
    pub const SUBSCRIPTION: &str = gts_id!("cf.core.events.subscription.v1~");
    pub const PRODUCER: &str = gts_id!("cf.core.events.producer.v1~");
    pub const PARTITION: &str = gts_id!("cf.core.events.partition.v1~");
    pub const EVENT: &str = gts_id!("cf.core.events.event.v1~");
    /// The event-*type* resource (distinct from [`EVENT`], an event instance).
    /// A not-found on an event type is stamped and decoded on this id, matching
    /// the gear's `EventTypeResourceError`, so an unknown event type survives
    /// the wire round-trip as [`super::EventBrokerError::EventTypeUnknown`]
    /// rather than degrading to `Other`.
    pub const EVENT_TYPE: &str = gts_id!("cf.core.events.event_type.v1~");
    pub const STREAM: &str = gts_id!("cf.core.events.stream.v1~");
    pub const STORAGE: &str = gts_id!("cf.core.events.storage.v1~");
    pub const OFFSET: &str = gts_id!("cf.core.events.offset.v1~");
    pub const TRANSPORT: &str = gts_id!("cf.core.events.transport.v1~");
    pub const REQUEST: &str = gts_id!("cf.core.events.request.v1~");
}

pub mod reasons {
    pub const INVALID_PRODUCER_OPTIONS: &str = "invalid_producer_options";
    pub const INVALID_CONSUMER_OPTIONS: &str = "invalid_consumer_options";
    pub const EVENT_TYPE_NOT_DECLARED: &str = "event_type_not_declared";
    pub const INVALID_EVENT_FIELD: &str = "invalid_event_field";
    /// A text value carried a byte outside printable ASCII.
    pub const ASCII_ONLY: &str = "ascii_only";
    /// A text value fell outside its field's byte bounds.
    pub const FIELD_TOO_LONG: &str = "field_too_long";
    /// A `client_agent` broke the RFC 9110 User-Agent grammar.
    pub const USER_AGENT_GRAMMAR: &str = "user_agent_grammar";
    pub const EVENT_DATA_INVALID: &str = "event_data_invalid";
    /// An event's data is not well-formed JSON. SDK-local, like the three
    /// decode reasons below: the broker never answers them.
    pub const DECODE_SYNTAX: &str = "decode_syntax";
    /// An event's data is well-formed but does not fit the payload type.
    pub const DECODE_DATA: &str = "decode_data";
    /// An event's data ends before a complete value.
    pub const DECODE_EOF: &str = "decode_eof";
    /// Reading an event's data failed.
    pub const DECODE_IO: &str = "decode_io";
    pub const BATCH_TOO_LARGE: &str = "batch_too_large";
    /// An event encoded to more bytes than the per-event limit admits.
    pub const EVENT_TOO_LARGE: &str = "event_too_large";
    /// An event's resolved partition key took more bytes than its cap admits.
    pub const PARTITION_KEY_TOO_LONG: &str = "partition_key_too_long";
    pub const INVALID_BACKEND_CONFIG: &str = "invalid_backend_config";
    pub const TYPE_NOT_IN_DECLARED_TOPIC: &str = "type_not_in_declared_topic";
    pub const SCHEMA_NOT_PREPARED: &str = "schema_not_prepared";
    pub const CONSUMER_GROUP_HAS_ACTIVE_MEMBERS: &str = "consumer_group_has_active_members";
    pub const SEQUENCE_MISMATCH: &str = "sequence_mismatch";
    /// A direct producer's resend found its next producer sequence taken too.
    /// SDK-local: the broker never answers it.
    pub const SEQUENCE_TAKEN: &str = "sequence_taken";
    pub const POSITIONS_NOT_SET: &str = "positions_not_set";
    pub const PARTITION_NOT_ASSIGNED: &str = "partition_not_assigned";
    /// A seek's `topology_version` differed from the subscription's
    /// current one: the group rebalanced under the caller, who must re-read the
    /// subscription and re-seek.
    pub const TOPOLOGY_VERSION_MISMATCH: &str = "topology_version_mismatch";
    pub const STREAMING_IN_PROGRESS: &str = "streaming_in_progress";
    pub const IN_TX_OFFSETS_NOT_SUPPORTED: &str = "in_tx_offsets_not_supported";
    pub const RATE_LIMIT: &str = "event-broker.rate-limit";
    pub const CONSUMER_GROUP_CAPACITY: &str = "event-broker.consumer-group-capacity";
    pub const UNAUTHORIZED: &str = "event_broker_unauthorized";
    pub const STORAGE_UNAVAILABLE: &str = "storage_unavailable";
    pub const OFFSET_MANAGER_UNAVAILABLE: &str = "offset_manager_unavailable";
    /// A seek position sat below the lowest position the partition admits.
    pub const BELOW_RETENTION_FLOOR: &str = "below_retention_floor";
    /// A seek position sat above the partition's high-water mark.
    pub const ABOVE_HIGH_WATER_MARK: &str = "above_high_water_mark";
    pub const TRANSPORT_UNAVAILABLE: &str = "transport_unavailable";
}

/// Which end of a partition's admissible range a requested position fell
/// outside of.
///
/// A position is admissible when it sits in `[floor, ceiling]` - the position
/// below the oldest surviving event, up to the highest sequence assigned. The
/// two ends fail for different reasons and a caller corrects them differently,
/// so a rejection says which one it was rather than only that the range was
/// missed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutOfRange {
    /// Below the floor: retention has removed everything down there, so no
    /// event remains for delivery to resume from.
    BelowFloor,
    /// Above the ceiling: nothing has been assigned that high yet.
    AboveCeiling,
}

impl OutOfRange {
    /// The machine reason a problem body carries for this end.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::BelowFloor => reasons::BELOW_RETENTION_FLOOR,
            Self::AboveCeiling => reasons::ABOVE_HIGH_WATER_MARK,
        }
    }

    /// How the end reads in a sentence describing the range that was missed.
    #[must_use]
    pub const fn phrase(self) -> &'static str {
        match self {
            Self::BelowFloor => "below",
            Self::AboveCeiling => "above",
        }
    }
}

/// One entry of a seek request whose position its partition does not admit.
///
/// Carries which entry it was (its `positions` topic key and partition, both
/// validated), the range that partition admits, and which end was crossed - and
/// deliberately **not** the submitted position value. The topic and partition
/// locate the entry the caller sent; the range it names is broker state, not
/// request content.
///
/// [`Self::as_field_violation`] is the single place this becomes a problem-body
/// entry, so every implementation of the API rejects the same input with the
/// same body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PositionViolation {
    topic: String,
    partition: i32,
    floor: Sequence,
    ceiling: Sequence,
    breached: OutOfRange,
}

impl PositionViolation {
    /// Starts a violation for the entry the request placed under `topic` at
    /// `partition` in its `positions` map.
    #[must_use]
    pub fn builder(topic: impl Into<String>, partition: i32) -> PositionViolationBuilder {
        PositionViolationBuilder {
            topic: topic.into(),
            partition,
            floor: Sequence::NONE,
            ceiling: Sequence::NONE,
            breached: OutOfRange::BelowFloor,
        }
    }

    #[must_use]
    pub const fn breached(&self) -> OutOfRange {
        self.breached
    }

    /// The problem-body entry for this violation. The field is the stable
    /// request member `positions` - never the topic, so no user value lands in
    /// a field path. The entry it concerns (topic, partition) rides in the
    /// description, where the topic is safe to name: it is a `GtsInstanceId`
    /// validated at the API boundary before any `PositionViolation` exists.
    /// Neither reproduces the submitted position value - the range it names is
    /// broker state.
    #[must_use]
    pub fn as_field_violation(&self) -> FieldViolation {
        FieldViolation::new(
            "positions",
            format!(
                "topic {} partition {}: the seek position is {} the valid range [{}, {}]",
                self.topic,
                self.partition,
                self.breached.phrase(),
                self.floor,
                self.ceiling
            ),
            self.breached.reason(),
        )
    }
}

/// Builds a [`PositionViolation`]. The two ends of the range are set by name,
/// because a positional pair of sequences transposes silently.
#[derive(Debug, Clone)]
pub struct PositionViolationBuilder {
    topic: String,
    partition: i32,
    floor: Sequence,
    ceiling: Sequence,
    breached: OutOfRange,
}

impl PositionViolationBuilder {
    /// The lowest position the partition admits.
    #[must_use]
    pub const fn floor(mut self, floor: Sequence) -> Self {
        self.floor = floor;
        self
    }

    /// The highest position the partition admits.
    #[must_use]
    pub const fn ceiling(mut self, ceiling: Sequence) -> Self {
        self.ceiling = ceiling;
        self
    }

    /// Which end the requested position fell outside of.
    #[must_use]
    pub const fn breached(mut self, breached: OutOfRange) -> Self {
        self.breached = breached;
        self
    }

    #[must_use]
    pub fn build(self) -> PositionViolation {
        PositionViolation {
            topic: self.topic,
            partition: self.partition,
            floor: self.floor,
            ceiling: self.ceiling,
            breached: self.breached,
        }
    }
}

/// Which kind of failure a payload decode hit, as serde_json classifies it. It carries
/// nothing from the data: serde's own message can quote the offending value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeCategory {
    /// The data is not well-formed JSON.
    Syntax,
    /// The data is well-formed but does not fit the payload type.
    Data,
    /// The data ends before a complete value.
    Eof,
    /// Reading the data failed.
    Io,
}

impl From<serde_json::error::Category> for DecodeCategory {
    fn from(category: serde_json::error::Category) -> Self {
        match category {
            serde_json::error::Category::Syntax => Self::Syntax,
            serde_json::error::Category::Data => Self::Data,
            serde_json::error::Category::Eof => Self::Eof,
            serde_json::error::Category::Io => Self::Io,
        }
    }
}

impl DecodeCategory {
    /// The machine reason the canonical form of a decode error carries for
    /// this class.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Syntax => reasons::DECODE_SYNTAX,
            Self::Data => reasons::DECODE_DATA,
            Self::Eof => reasons::DECODE_EOF,
            Self::Io => reasons::DECODE_IO,
        }
    }
}

impl std::fmt::Display for DecodeCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Syntax => "syntax",
            Self::Data => "data",
            Self::Eof => "eof",
            Self::Io => "io",
        })
    }
}

/// Which of the SDK's transactional outboxes an [`EventBrokerError::Outbox`]
/// error comes from. Its text starts the error's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxRole {
    /// The producer outbox of an `OutboxSetup` and its `OutboxProducer`.
    Producer,
    /// A consumer's dead-letter outbox, `ConsumerDlqOutbox`.
    DeadLetter,
}

impl std::fmt::Display for OutboxRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Producer => "outbox",
            Self::DeadLetter => "dead-letter outbox",
        })
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum EventBrokerError {
    #[error("invalid producer options: {detail}")]
    InvalidProducerOptions { detail: String },

    #[error("invalid consumer options: {detail}")]
    InvalidConsumerOptions { detail: String },

    #[error("event type not declared on producer: {event_type}")]
    EventTypeNotDeclared { event_type: String, detail: String },

    #[error("event type unknown to types-registry: {event_type}")]
    EventTypeUnknown { event_type: String, detail: String },

    #[error("resolved type's parent topic differs from declared topics: {event_type}")]
    TypeNotInDeclaredTopic {
        event_type: String,
        expected_topic: String,
        detail: String,
    },

    #[error(
        "schema not prepared for {event_type}; call `prepare_event_type` before opening the txn"
    )]
    SchemaNotPrepared { event_type: String, detail: String },

    #[error("event field invalid: {field}: {detail}")]
    InvalidEventField { field: &'static str, detail: String },

    /// A text field broke its encoding or length rule. Carries no part of the
    /// submitted value - only the field, what the rule requires, and a
    /// machine-readable reason.
    #[error("text field invalid: {field}: {detail}")]
    InvalidTextField {
        field: &'static str,
        detail: String,
        reason: &'static str,
    },

    /// An event's `data` did not decode as the payload type asked for. Names the type and
    /// the failure class, and carries no part of the data.
    #[error("event data of {event_type} does not decode as the payload type ({category})")]
    Decode {
        event_type: String,
        category: DecodeCategory,
    },

    #[error("event data invalid for {event_type}: {errors:?}")]
    EventDataInvalid {
        event_type: String,
        errors: Vec<String>,
        detail: String,
    },

    #[error("topic not found: {topic}")]
    TopicNotFound { topic: String, detail: String },

    #[error("consumer group not found")]
    ConsumerGroupNotFound {
        /// The group's GTS id as the broker sent it.
        consumer_group: String,
        detail: String,
    },

    #[error("consumer group has active members")]
    ConsumerGroupHasActiveMembers { detail: String },

    #[error("subscription not found: {id}")]
    SubscriptionNotFound { id: SubscriptionId, detail: String },

    #[error("not authorized: {detail}")]
    Unauthorized { detail: String },

    #[error("unknown producer: {producer:?}")]
    UnknownProducer {
        producer: crate::ids::ProducerId,
        detail: String,
    },

    #[error("sequence violation (broker expects previous={expected_previous})")]
    SequenceViolation {
        expected_previous: i64,
        detail: String,
    },

    /// A `DirectProducer` publish stamped with fresh numbers was answered
    /// `Duplicate` twice: an earlier request whose outcome the producer did not
    /// see took those numbers, and then the next ones too. Neither send stored
    /// the publish's events; the next publish on the topic re-reads the head.
    /// SDK-local: the broker never answers it.
    #[error("the next producer sequence on topic {topic} is already taken")]
    SequenceTaken { topic: String },

    /// A `DirectProducer` publish found its topic's `max_waiting_per_topic`
    /// publishes already queued for or holding the topic's lock, and failed at
    /// once without sending anything. SDK-local: the broker never answers it.
    #[error("too many publishes wait on one topic of this producer (limit {limit})")]
    ProducerBusy { limit: usize },

    /// An `OutboxProducer` was bound to a handle whose outbox has no queue of
    /// its name, or has it with fewer partitions than an event's partition:
    /// bind the handle started from `OutboxSetup::register`'s builder. Nothing
    /// was written. SDK-local: the broker never answers it.
    #[error("the outbox the producer is bound to has no queue {queue} with its partitions")]
    OutboxQueueMismatch { queue: String },

    #[error("rate limit exceeded, retry after {retry_after_secs}s")]
    RateLimitExceeded {
        retry_after_secs: u32,
        detail: String,
    },

    #[error("consumer group at capacity ({active} active members, {partitions} partitions)")]
    GroupAtCapacity {
        active: u32,
        partitions: u32,
        detail: String,
    },

    #[error("publish rate limited (429), retry after {retry_after_secs}s")]
    RateLimited {
        retry_after_secs: u32,
        detail: String,
    },

    #[error(
        "batch too large: {count} events / {bytes} bytes (limits: {max_count} events, {max_bytes} bytes)"
    )]
    BatchTooLarge {
        count: usize,
        bytes: usize,
        max_count: usize,
        max_bytes: usize,
        detail: String,
    },

    /// An event encodes to more than [`crate::gts::EventV1::MAX_ENCODED_BYTES`]. The
    /// broker answers it `413`, and a producer refuses such an event before it sends or
    /// enqueues it.
    #[error("event too large: {bytes} bytes encoded (max {max})")]
    EventTooLarge { bytes: usize, max: usize },

    /// An event's resolved partition key takes more than
    /// [`crate::gts::EventV1::MAX_PARTITION_KEY_BYTES`]. The broker answers it `400`, and
    /// a producer refuses such an event before it sends or enqueues it. Carries no part
    /// of the key.
    #[error("partition key too long: {bytes} bytes (max {max})")]
    PartitionKeyTooLong { bytes: usize, max: usize },

    #[error("subscription recovery exhausted ({attempts} consecutive re-JOIN failures)")]
    SubscriptionRecoveryExhausted { attempts: u32, detail: String },

    /// One or more seek entries named a position their partition does not
    /// admit. Every offender is reported, so a caller correcting several does
    /// not need one round trip each.
    #[error("{} seek position(s) outside the admissible range", violations.len())]
    InvalidInitialPosition {
        violations: Vec<PositionViolation>,
        detail: String,
    },

    #[error("positions not set for {} partition(s): {}", unseeded.len(), display_unseeded(unseeded))]
    PositionsNotSet {
        unseeded: Vec<(String, u32)>,
        detail: String,
    },

    #[error("partition {topic}:{partition} is not assigned to this subscription")]
    PartitionNotAssigned {
        topic: String,
        partition: u32,
        detail: String,
    },

    /// The seek's `topology_version` did not match the subscription's
    /// current one - the group rebalanced under the caller. A stateless
    /// re-read signal: it carries no topology version and no assignment, because
    /// the caller recovers by re-reading the subscription (`GET
    /// /v1/subscriptions/{id}`), which is the single source of truth for the
    /// fresh version and assignment, not this error.
    #[error("subscription topology changed; re-read the subscription and re-seek")]
    TopologyVersionMismatch { detail: String },

    #[error("a stream is already open for this subscription")]
    StreamingInProgress { detail: String },

    #[error("storage backend error")]
    Backend(#[from] BackendError),

    #[error("offset manager error")]
    OffsetManager(#[from] OffsetManagerError),

    /// A transactional outbox of the SDK failed: the producer's or a
    /// consumer's dead-letter outbox, as `role` says. The source is the
    /// database error itself for a database failure, and the outbox error
    /// otherwise. The text carries the source's: the outbox's names queues,
    /// partitions and sizes, and a database error's is the driver's message,
    /// which can quote values.
    #[error("{role}: {source}")]
    Outbox {
        role: OutboxRole,
        #[source]
        source: Arc<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[error("transport: {0}")]
    Transport(String),

    #[error("internal: {0}")]
    Internal(String),

    /// A recognized-but-unbuilt capability was requested (e.g. a named consumer
    /// group). Maps to canonical `Unimplemented` (wire 501).
    #[error("not implemented: {0}")]
    Unimplemented(String),

    #[error("{canonical}")]
    Other { canonical: CanonicalError },
}

pub type ConsumerError = EventBrokerError;

impl EventBrokerError {
    /// Wire form of a text-field rejection from [`crate::validate`].
    ///
    /// Carries the field, the rule's requirement and its machine reason, and
    /// deliberately nothing from the submitted value.
    #[must_use]
    pub fn invalid_text_field(error: &crate::validate::TextFieldError) -> Self {
        EventBrokerError::InvalidTextField {
            field: error.field(),
            detail: error.detail().to_owned(),
            reason: error.rule().as_reason(),
        }
    }

    /// The variant's name, for logs and stored reject texts. It carries no
    /// field value.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::InvalidProducerOptions { .. } => "InvalidProducerOptions",
            Self::InvalidConsumerOptions { .. } => "InvalidConsumerOptions",
            Self::EventTypeNotDeclared { .. } => "EventTypeNotDeclared",
            Self::EventTypeUnknown { .. } => "EventTypeUnknown",
            Self::TypeNotInDeclaredTopic { .. } => "TypeNotInDeclaredTopic",
            Self::SchemaNotPrepared { .. } => "SchemaNotPrepared",
            Self::InvalidEventField { .. } => "InvalidEventField",
            Self::InvalidTextField { .. } => "InvalidTextField",
            Self::Decode { .. } => "Decode",
            Self::EventDataInvalid { .. } => "EventDataInvalid",
            Self::TopicNotFound { .. } => "TopicNotFound",
            Self::ConsumerGroupNotFound { .. } => "ConsumerGroupNotFound",
            Self::ConsumerGroupHasActiveMembers { .. } => "ConsumerGroupHasActiveMembers",
            Self::SubscriptionNotFound { .. } => "SubscriptionNotFound",
            Self::Unauthorized { .. } => "Unauthorized",
            Self::UnknownProducer { .. } => "UnknownProducer",
            Self::SequenceViolation { .. } => "SequenceViolation",
            Self::SequenceTaken { .. } => "SequenceTaken",
            Self::ProducerBusy { .. } => "ProducerBusy",
            Self::OutboxQueueMismatch { .. } => "OutboxQueueMismatch",
            Self::RateLimitExceeded { .. } => "RateLimitExceeded",
            Self::GroupAtCapacity { .. } => "GroupAtCapacity",
            Self::RateLimited { .. } => "RateLimited",
            Self::BatchTooLarge { .. } => "BatchTooLarge",
            Self::EventTooLarge { .. } => "EventTooLarge",
            Self::PartitionKeyTooLong { .. } => "PartitionKeyTooLong",
            Self::SubscriptionRecoveryExhausted { .. } => "SubscriptionRecoveryExhausted",
            Self::InvalidInitialPosition { .. } => "InvalidInitialPosition",
            Self::PositionsNotSet { .. } => "PositionsNotSet",
            Self::PartitionNotAssigned { .. } => "PartitionNotAssigned",
            Self::TopologyVersionMismatch { .. } => "TopologyVersionMismatch",
            Self::StreamingInProgress { .. } => "StreamingInProgress",
            Self::Backend(_) => "Backend",
            Self::OffsetManager(_) => "OffsetManager",
            Self::Outbox { .. } => "Outbox",
            Self::Transport(_) => "Transport",
            Self::Internal(_) => "Internal",
            Self::Unimplemented(_) => "Unimplemented",
            Self::Other { .. } => "Other",
        }
    }
}

/// The fields of the two `batch_too_large` violations, one per bound.
const BATCH_COUNT_FIELD: &str = "batch.count";
const BATCH_BYTES_FIELD: &str = "batch.bytes";

/// The quota-violation subject of the canonical `ProducerBusy` form, the
/// marker the reverse mapping refuses to recover it by.
const PRODUCER_BUSY_SUBJECT: &str = "max_waiting_per_topic";

impl From<EventBrokerError> for CanonicalError {
    fn from(err: EventBrokerError) -> Self {
        match err {
            EventBrokerError::InvalidProducerOptions { detail, .. } => {
                ProducerOptionsResourceError::invalid_argument()
                    .with_field_violation(
                        "producer_options",
                        detail,
                        reasons::INVALID_PRODUCER_OPTIONS,
                    )
                    .create()
            }
            EventBrokerError::InvalidConsumerOptions { detail, .. } => {
                ConsumerOptionsResourceError::invalid_argument()
                    .with_field_violation(
                        "consumer_options",
                        detail,
                        reasons::INVALID_CONSUMER_OPTIONS,
                    )
                    .create()
            }
            EventBrokerError::EventTypeNotDeclared {
                event_type, detail, ..
            } => EventResourceError::invalid_argument()
                .with_resource(event_type)
                .with_field_violation("event_type", detail, reasons::EVENT_TYPE_NOT_DECLARED)
                .create(),
            // The only arm carrying no reason: a 404 whose `resource` names the
            // unresolvable type already says everything a caller can act on, and
            // no other arm reports not-found on this resource.
            EventBrokerError::EventTypeUnknown {
                event_type, detail, ..
            } => EventTypeResourceError::not_found(detail)
                .with_resource(event_type)
                .create(),
            EventBrokerError::TypeNotInDeclaredTopic {
                event_type,
                expected_topic,
                detail,
                ..
            } => EventResourceError::failed_precondition()
                .with_resource(event_type.clone())
                .with_precondition_violation(
                    event_type,
                    format!("{detail}; expected topic {expected_topic}"),
                    reasons::TYPE_NOT_IN_DECLARED_TOPIC,
                )
                .create(),
            EventBrokerError::SchemaNotPrepared {
                event_type, detail, ..
            } => EventResourceError::failed_precondition()
                .with_resource(event_type.clone())
                .with_precondition_violation(event_type, detail, reasons::SCHEMA_NOT_PREPARED)
                .create(),
            EventBrokerError::InvalidEventField { field, detail, .. } => {
                EventResourceError::invalid_argument()
                    .with_field_violation(field, detail, reasons::INVALID_EVENT_FIELD)
                    .create()
            }
            EventBrokerError::InvalidTextField {
                field,
                detail,
                reason,
                ..
            } => RequestResourceError::invalid_argument()
                .with_field_violation(field, detail, reason)
                .create(),
            // SDK-local: a decode happens in the caller's process, so this never travels
            // and the reverse mapping does not recover it.
            EventBrokerError::Decode {
                event_type,
                category,
            } => {
                let description = format!(
                    "event data of {event_type} does not decode as the payload type ({category})"
                );
                EventResourceError::invalid_argument()
                    .with_resource(event_type)
                    .with_field_violation("data", description, category.reason())
                    .create()
            }
            EventBrokerError::EventDataInvalid {
                event_type,
                errors,
                detail,
                ..
            } => {
                let description = if errors.is_empty() {
                    detail
                } else {
                    format!("{detail}: {}", errors.join("; "))
                };
                EventResourceError::invalid_argument()
                    .with_resource(event_type)
                    .with_field_violation("data", description, reasons::EVENT_DATA_INVALID)
                    .create()
            }
            EventBrokerError::TopicNotFound { topic, detail, .. } => {
                TopicResourceError::not_found(detail)
                    .with_resource(topic)
                    .create()
            }
            EventBrokerError::ConsumerGroupNotFound {
                consumer_group,
                detail,
                ..
            } => ConsumerGroupResourceError::not_found(detail)
                .with_resource(consumer_group)
                .create(),
            EventBrokerError::ConsumerGroupHasActiveMembers { detail, .. } => {
                ConsumerGroupResourceError::failed_precondition()
                    .with_precondition_violation(
                        resources::CONSUMER_GROUP,
                        detail,
                        reasons::CONSUMER_GROUP_HAS_ACTIVE_MEMBERS,
                    )
                    .create()
            }
            EventBrokerError::SubscriptionNotFound { id, detail, .. } => {
                SubscriptionResourceError::not_found(detail)
                    .with_resource(id.to_string())
                    .create()
            }
            EventBrokerError::Unauthorized { detail, .. } => {
                let _ = detail;
                EventResourceError::permission_denied()
                    .with_reason(reasons::UNAUTHORIZED)
                    .create()
            }
            EventBrokerError::UnknownProducer {
                producer, detail, ..
            } => ProducerResourceError::not_found(detail)
                .with_resource(producer.0.to_string())
                .create(),
            EventBrokerError::SequenceViolation {
                expected_previous,
                detail,
                ..
            } => ProducerResourceError::failed_precondition()
                .with_precondition_violation(
                    "producer_sequence",
                    format!("{detail}; expected previous {expected_previous}"),
                    reasons::SEQUENCE_MISMATCH,
                )
                .create(),
            // A concurrency conflict the caller may retry: re-publishing the
            // event stamps it from a re-read head.
            EventBrokerError::SequenceTaken { topic } => TopicResourceError::aborted(format!(
                "the next producer sequence on topic {topic} is already taken"
            ))
            .with_resource(topic)
            .with_reason(reasons::SEQUENCE_TAKEN)
            .create(),
            // Back-pressure from this producer, never on the wire: no retry
            // hint, and no reason key, since the reverse mapping does not
            // recover it. A quota violation has no numeric slot, so the limit
            // rides in its description, under the subject naming the option.
            EventBrokerError::ProducerBusy { limit } => {
                let detail = format!(
                    "too many publishes wait on one topic of this producer (limit {limit})"
                );
                ProducerResourceError::resource_exhausted(detail.clone())
                    .with_quota_violation(PRODUCER_BUSY_SUBJECT, detail)
                    .create()
            }
            // A wiring fault in this process, never on the wire: the reverse
            // mapping reads it back as `Internal`, not as this variant. An
            // internal error has no resource slot, so the queue rides in the
            // description, the server-side diagnostic.
            EventBrokerError::OutboxQueueMismatch { queue } => CanonicalError::internal(format!(
                "the outbox the producer is bound to has no queue {queue} with its partitions"
            ))
            .create(),
            EventBrokerError::RateLimitExceeded {
                retry_after_secs,
                detail,
                ..
            }
            | EventBrokerError::RateLimited {
                retry_after_secs,
                detail,
                ..
            } => EventResourceError::resource_exhausted(detail.clone())
                .with_quota_violation(reasons::RATE_LIMIT, detail)
                .with_quota_violation_retry_after_seconds(u64::from(retry_after_secs))
                .create(),
            EventBrokerError::GroupAtCapacity {
                active,
                partitions,
                detail,
                ..
            } => ConsumerGroupResourceError::resource_exhausted(detail.clone())
                .with_quota_violation(
                    reasons::CONSUMER_GROUP_CAPACITY,
                    format!("{detail}; active={active}; partitions={partitions}"),
                )
                .create(),
            // The broker's body: one violation, on the bound that broke, the count
            // checked first as the broker checks it. `detail` does not travel.
            EventBrokerError::BatchTooLarge {
                count,
                bytes,
                max_count,
                max_bytes,
                ..
            } => {
                let (field, description) = if count > max_count {
                    (
                        BATCH_COUNT_FIELD,
                        format!("batch too large: {count} events (max {max_count})"),
                    )
                } else {
                    (
                        BATCH_BYTES_FIELD,
                        format!("batch payload too large: {bytes} bytes (max {max_bytes})"),
                    )
                };
                RequestResourceError::invalid_argument()
                    .with_field_violation(field, description, reasons::BATCH_TOO_LARGE)
                    .with_override(Http::status_code(413))
                    .create()
            }
            // The broker's form names the event's batch position in `field`; the SDK's
            // check runs per event and has none.
            EventBrokerError::EventTooLarge { bytes, max } => {
                RequestResourceError::invalid_argument()
                    .with_field_violation(
                        "event",
                        format!("{bytes} bytes encoded, over the limit of {max}"),
                        reasons::EVENT_TOO_LARGE,
                    )
                    .with_override(Http::status_code(413))
                    .create()
            }
            EventBrokerError::PartitionKeyTooLong { bytes, max } => {
                RequestResourceError::invalid_argument()
                    .with_field_violation(
                        "partition_key",
                        format!("{bytes} bytes, over the limit of {max}"),
                        reasons::PARTITION_KEY_TOO_LONG,
                    )
                    .create()
            }
            EventBrokerError::SubscriptionRecoveryExhausted { .. } => {
                CanonicalError::service_unavailable().create()
            }
            EventBrokerError::InvalidInitialPosition {
                violations, detail, ..
            } => {
                let mut iter = violations.iter().map(PositionViolation::as_field_violation);
                // Non-empty by construction - the error exists because an
                // entry was rejected - but the builder's typestate needs the
                // first violation separately, so the empty case is spelled out.
                let Some(first) = iter.next() else {
                    return RequestResourceError::invalid_argument()
                        .with_format(detail)
                        .create();
                };
                // The offending thing is a field of the request, not a
                // partition: the violation names an entry in the `positions`
                // map, and the gear's own SEEK rejection
                // stamps the same resource. A rejection that differed here
                // between an implementation and its reference would be a
                // difference a caller can see.
                let mut with_violations = RequestResourceError::invalid_argument()
                    .with_field_violation(first.field, first.description, first.reason);
                for violation in iter {
                    with_violations = with_violations.with_field_violation(
                        violation.field,
                        violation.description,
                        violation.reason,
                    );
                }
                with_violations.create()
            }
            EventBrokerError::PositionsNotSet {
                unseeded, detail, ..
            } => {
                let mut iter = unseeded.into_iter();
                let Some((topic, partition)) = iter.next() else {
                    return StreamResourceError::failed_precondition()
                        .with_precondition_violation(
                            "stream.positions",
                            detail,
                            reasons::POSITIONS_NOT_SET,
                        )
                        .create();
                };
                let mut with_violations = StreamResourceError::failed_precondition()
                    .with_precondition_violation(
                        format!("{topic}:{partition}"),
                        detail.clone(),
                        reasons::POSITIONS_NOT_SET,
                    );
                for (topic, partition) in iter {
                    with_violations = with_violations.with_precondition_violation(
                        format!("{topic}:{partition}"),
                        detail.clone(),
                        reasons::POSITIONS_NOT_SET,
                    );
                }
                with_violations.create()
            }
            EventBrokerError::PartitionNotAssigned {
                topic,
                partition,
                detail,
                ..
            } => PartitionResourceError::failed_precondition()
                .with_resource(format!("{topic}:{partition}"))
                .with_precondition_violation(
                    format!("{topic}:{partition}"),
                    detail,
                    reasons::PARTITION_NOT_ASSIGNED,
                )
                .create(),
            EventBrokerError::TopologyVersionMismatch { detail, .. } => {
                // Minimal body: the subscription resource + the reason, and no
                // topology version or assignment - the caller re-reads the
                // subscription for the fresh state.
                SubscriptionResourceError::failed_precondition()
                    .with_precondition_violation(
                        resources::SUBSCRIPTION,
                        detail,
                        reasons::TOPOLOGY_VERSION_MISMATCH,
                    )
                    .create()
            }
            EventBrokerError::StreamingInProgress { detail, .. } => {
                StreamResourceError::failed_precondition()
                    .with_precondition_violation(
                        resources::STREAM,
                        detail,
                        reasons::STREAMING_IN_PROGRESS,
                    )
                    .create()
            }
            EventBrokerError::Backend(err) => CanonicalError::from(err),
            EventBrokerError::OffsetManager(err) => CanonicalError::from(err),
            // The text goes only into the server-side diagnostic, which never
            // reaches the wire: a database error's text can quote values.
            EventBrokerError::Outbox { role, source } => {
                CanonicalError::internal(format!("{role}: {source}")).create()
            }
            EventBrokerError::Transport(_) => CanonicalError::service_unavailable().create(),
            EventBrokerError::Internal(detail) => CanonicalError::internal(detail).create(),
            EventBrokerError::Unimplemented(detail) => {
                ConsumerGroupResourceError::unimplemented(detail).create()
            }
            EventBrokerError::Other { canonical } => canonical,
        }
    }
}

/// First field-violation reason on an `InvalidArgument`/`OutOfRange` context, if
/// the context carries field violations at all.
fn first_invalid_argument_reason(
    ctx: &toolkit_canonical_errors::context::InvalidArgument,
) -> Option<&str> {
    match ctx {
        toolkit_canonical_errors::context::InvalidArgumentV1::FieldViolations {
            field_violations,
        } => field_violations.first().map(|v| v.reason.as_str()),
        _ => None,
    }
}

#[cfg(feature = "db")]
impl EventBrokerError {
    /// An error of the outbox `role` names, kept as the source of
    /// [`EventBrokerError::Outbox`]. For a database error the source is the
    /// `sea_orm::DbErr` itself, since the outbox's `Database` variant is
    /// transparent and would hide it from a chain walk.
    pub(crate) fn outbox(role: OutboxRole, err: toolkit_db::outbox::OutboxError) -> Self {
        let source: Arc<dyn std::error::Error + Send + Sync + 'static> = match err {
            toolkit_db::outbox::OutboxError::Database(database) => Arc::new(database),
            err => Arc::new(err),
        };
        Self::Outbox { role, source }
    }
}

/// Maps the errors of the outbox an `OutboxProducer` writes to. A handle from
/// another outbox (no queue of the producer's name, or too few partitions) is
/// [`EventBrokerError::OutboxQueueMismatch`]; anything else is
/// [`EventBrokerError::Outbox`] of [`OutboxRole::Producer`], whose source is
/// the `sea_orm::DbErr` itself for a database error and the outbox error
/// otherwise.
#[cfg(feature = "db")]
impl From<toolkit_db::outbox::OutboxError> for EventBrokerError {
    fn from(err: toolkit_db::outbox::OutboxError) -> Self {
        use toolkit_db::outbox::OutboxError;

        match err {
            OutboxError::QueueNotRegistered(queue)
            | OutboxError::PartitionOutOfRange { queue, .. } => Self::OutboxQueueMismatch { queue },
            err => Self::outbox(OutboxRole::Producer, err),
        }
    }
}

/// Inverse of [`From<EventBrokerError> for CanonicalError`]. Recovers the typed
/// SDK error from a canonical error decoded off the wire, keyed on the category,
/// the `resource_type`, and the first violation's reason - the same identity the
/// forward mapping stamps.
///
/// Variants whose payload is not fully carried on the wire (for example
/// `InvalidEventField`/`InvalidTextField`, whose `&'static str` field name is on the
/// wire but cannot be rebuilt as a `&'static str`, or the counted `active`/`partitions`
/// in `GroupAtCapacity`) are recovered to the right variant with the wire-available
/// fields; fields absent from the wire take a neutral value. Anything it does not
/// recover, the forms of the SDK-local `SequenceTaken` and `ProducerBusy` included,
/// falls to [`EventBrokerError::Other`], which preserves the canonical error verbatim.
/// Of the `ResourceExhausted` errors on the producer resource, only the
/// `ProducerBusy` form (first violation subject `max_waiting_per_topic`) is not
/// recovered; any other reads back as [`EventBrokerError::RateLimitExceeded`]
/// with its retry hint.
/// The SDK-local `OutboxQueueMismatch` and `Outbox` travel as `Internal` and
/// read back as [`EventBrokerError::Internal`] with the canonical internal
/// detail.
impl From<CanonicalError> for EventBrokerError {
    fn from(canonical: CanonicalError) -> Self {
        let detail = canonical.detail().to_owned();
        match &canonical {
            CanonicalError::PermissionDenied { .. } => Self::Unauthorized { detail },

            CanonicalError::NotFound {
                resource_type,
                resource_name,
                ..
            } => {
                let name = resource_name.clone().unwrap_or_default();
                match resource_type.as_deref() {
                    Some(resources::TOPIC) => Self::TopicNotFound {
                        topic: name,
                        detail,
                    },
                    Some(resources::CONSUMER_GROUP) => Self::ConsumerGroupNotFound {
                        consumer_group: name,
                        detail,
                    },
                    Some(resources::SUBSCRIPTION) => match uuid::Uuid::parse_str(&name) {
                        Ok(id) => Self::SubscriptionNotFound {
                            id: SubscriptionId(id),
                            detail,
                        },
                        Err(_) => Self::Other { canonical },
                    },
                    Some(resources::PRODUCER) => match uuid::Uuid::parse_str(&name) {
                        Ok(id) => Self::UnknownProducer {
                            producer: ProducerId(id),
                            detail,
                        },
                        Err(_) => Self::Other { canonical },
                    },
                    Some(resources::EVENT_TYPE) => Self::EventTypeUnknown {
                        event_type: name,
                        detail,
                    },
                    Some(resources::PARTITION) => {
                        Self::Backend(BackendError::PartitionNotFound { detail })
                    }
                    _ => Self::Other { canonical },
                }
            }

            CanonicalError::ResourceExhausted {
                ctx, resource_type, ..
            } => {
                let retry_after_secs = ctx
                    .violations
                    .first()
                    .and_then(|v| v.retry_after_seconds)
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or_default();
                match resource_type.as_deref() {
                    // active/partitions are not carried on the wire; the variant
                    // is recovered, the counts default.
                    Some(resources::CONSUMER_GROUP) => Self::GroupAtCapacity {
                        active: 0,
                        partitions: 0,
                        detail,
                    },
                    // The form of the SDK-local `ProducerBusy`, which is not
                    // recovered: a rate limit here would invite a retry loop.
                    // Any other limit on the producer is a rate limit.
                    Some(resources::PRODUCER)
                        if ctx
                            .violations
                            .first()
                            .is_some_and(|v| v.subject == PRODUCER_BUSY_SUBJECT) =>
                    {
                        Self::Other { canonical }
                    }
                    _ => Self::RateLimitExceeded {
                        retry_after_secs,
                        detail,
                    },
                }
            }

            CanonicalError::FailedPrecondition {
                ctx,
                resource_type,
                resource_name,
                ..
            } => {
                let reason = ctx.violations.first().map(|v| v.type_.as_str());
                let name = resource_name.clone().unwrap_or_default();
                match (resource_type.as_deref(), reason) {
                    (_, Some(reasons::SEQUENCE_MISMATCH)) => Self::SequenceViolation {
                        // The committed head rides last in the violation's
                        // description ("... last_sequence=<n>"), not the
                        // top-level detail, so recover it from there.
                        expected_previous: ctx
                            .violations
                            .first()
                            .map(|v| expected_previous_from_detail(&v.description))
                            .unwrap_or_default(),
                        detail,
                    },
                    (_, Some(reasons::CONSUMER_GROUP_HAS_ACTIVE_MEMBERS)) => {
                        Self::ConsumerGroupHasActiveMembers { detail }
                    }
                    (_, Some(reasons::TOPOLOGY_VERSION_MISMATCH)) => {
                        Self::TopologyVersionMismatch { detail }
                    }
                    (_, Some(reasons::STREAMING_IN_PROGRESS)) => {
                        Self::StreamingInProgress { detail }
                    }
                    (_, Some(reasons::IN_TX_OFFSETS_NOT_SUPPORTED)) => {
                        Self::OffsetManager(OffsetManagerError::InTxNotSupported { detail })
                    }
                    // The precondition violation carries no offender payload the
                    // wire preserves, so the partition list is empty and the
                    // caller re-reads the subscription for the real one.
                    (_, Some(reasons::POSITIONS_NOT_SET)) => Self::PositionsNotSet {
                        unseeded: Vec::new(),
                        detail,
                    },
                    (_, Some(reasons::PARTITION_NOT_ASSIGNED)) => Self::PartitionNotAssigned {
                        topic: String::new(),
                        partition: 0,
                        detail,
                    },
                    // The event type rides in `resource_name`; the expected
                    // topic is not carried, so it takes a neutral value.
                    (_, Some(reasons::TYPE_NOT_IN_DECLARED_TOPIC)) => {
                        Self::TypeNotInDeclaredTopic {
                            event_type: name,
                            expected_topic: String::new(),
                            detail,
                        }
                    }
                    (_, Some(reasons::SCHEMA_NOT_PREPARED)) => Self::SchemaNotPrepared {
                        event_type: name,
                        detail,
                    },
                    _ => Self::Other { canonical },
                }
            }

            CanonicalError::OutOfRange { ctx, .. } => {
                match ctx.field_violations.first().map(|v| v.reason.as_str()) {
                    Some(
                        reason @ (reasons::BELOW_RETENTION_FLOOR | reasons::ABOVE_HIGH_WATER_MARK),
                    ) => {
                        // Floor and ceiling ride in the violation description as
                        // free text, not as wire fields, so they take neutral
                        // values; only which end was breached survives, in the
                        // reason.
                        let breached = if reason == reasons::BELOW_RETENTION_FLOOR {
                            OutOfRange::BelowFloor
                        } else {
                            OutOfRange::AboveCeiling
                        };
                        Self::Backend(BackendError::OffsetOutOfRange {
                            floor: Sequence::NONE,
                            ceiling: Sequence::NONE,
                            breached,
                            detail,
                        })
                    }
                    _ => Self::Other { canonical },
                }
            }

            CanonicalError::InvalidArgument {
                ctx, resource_name, ..
            } => {
                let name = resource_name.clone().unwrap_or_default();
                match first_invalid_argument_reason(ctx) {
                    Some(reasons::INVALID_PRODUCER_OPTIONS) => {
                        Self::InvalidProducerOptions { detail }
                    }
                    Some(reasons::INVALID_CONSUMER_OPTIONS) => {
                        Self::InvalidConsumerOptions { detail }
                    }
                    // The wire names the field, but `field` is `&'static str`, so it
                    // is recovered empty.
                    Some(reasons::INVALID_EVENT_FIELD) => {
                        Self::InvalidEventField { field: "", detail }
                    }
                    Some(
                        reason @ (reasons::ASCII_ONLY
                        | reasons::FIELD_TOO_LONG
                        | reasons::USER_AGENT_GRAMMAR),
                    ) => {
                        let reason = match reason {
                            reasons::ASCII_ONLY => reasons::ASCII_ONLY,
                            reasons::FIELD_TOO_LONG => reasons::FIELD_TOO_LONG,
                            _ => reasons::USER_AGENT_GRAMMAR,
                        };
                        Self::InvalidTextField {
                            field: "",
                            detail,
                            reason,
                        }
                    }
                    // The event type rides in `resource_name`; the per-error
                    // list is not carried on the wire.
                    Some(reasons::EVENT_DATA_INVALID) => Self::EventDataInvalid {
                        event_type: name,
                        errors: Vec::new(),
                        detail,
                    },
                    Some(reasons::BATCH_TOO_LARGE) => Self::batch_too_large_from(ctx, detail),
                    Some(reasons::EVENT_TOO_LARGE) => {
                        let (bytes, max) = Self::size_from_violation(ctx);
                        Self::EventTooLarge { bytes, max }
                    }
                    Some(reasons::PARTITION_KEY_TOO_LONG) => {
                        let (bytes, max) = Self::size_from_violation(ctx);
                        Self::PartitionKeyTooLong { bytes, max }
                    }
                    // A seek position outside the admissible range renders as an
                    // `InvalidArgument` on the request resource (not `OutOfRange`,
                    // which the storage backend uses); the offender list is not
                    // carried on the wire.
                    Some(reasons::BELOW_RETENTION_FLOOR | reasons::ABOVE_HIGH_WATER_MARK) => {
                        Self::InvalidInitialPosition {
                            violations: Vec::new(),
                            detail,
                        }
                    }
                    _ => Self::Other { canonical },
                }
            }

            CanonicalError::Internal { .. } => Self::Internal(detail),
            CanonicalError::ServiceUnavailable { .. } => Self::Transport(detail),
            CanonicalError::Unimplemented { .. } => Self::Unimplemented(detail),
            _ => Self::Other { canonical },
        }
    }
}

impl EventBrokerError {
    /// The first field violation of an `InvalidArgument` context, if it carries any.
    fn first_field_violation(
        ctx: &toolkit_canonical_errors::context::InvalidArgument,
    ) -> Option<&FieldViolation> {
        match ctx {
            toolkit_canonical_errors::context::InvalidArgumentV1::FieldViolations {
                field_violations,
            } => field_violations.first(),
            _ => None,
        }
    }

    /// The measured size and the limit a size violation's description ends with, as
    /// its last two numbers (`"<bytes> bytes ..., over the limit of <max>"`), or zeros
    /// when the first field violation has no such description.
    fn size_from_violation(
        ctx: &toolkit_canonical_errors::context::InvalidArgument,
    ) -> (usize, usize) {
        let Some(violation) = Self::first_field_violation(ctx) else {
            return (0, 0);
        };
        let numbers = violation
            .description
            .split(|ch: char| !ch.is_ascii_digit())
            .filter_map(|part| part.parse::<usize>().ok())
            .collect::<Vec<_>>();
        match numbers.as_slice() {
            [.., bytes, max] => (*bytes, *max),
            _ => (0, 0),
        }
    }

    /// A `batch_too_large` refusal read back: the size and the limit of the bound its
    /// violation names, and zeros for the other bound, which the body does not carry.
    fn batch_too_large_from(
        ctx: &toolkit_canonical_errors::context::InvalidArgument,
        detail: String,
    ) -> Self {
        let (size, max) = Self::size_from_violation(ctx);
        let on_bytes = Self::first_field_violation(ctx)
            .is_some_and(|violation| violation.field == BATCH_BYTES_FIELD);
        if on_bytes {
            Self::BatchTooLarge {
                count: 0,
                bytes: size,
                max_count: 0,
                max_bytes: max,
                detail,
            }
        } else {
            Self::BatchTooLarge {
                count: size,
                bytes: 0,
                max_count: max,
                max_bytes: 0,
                detail,
            }
        }
    }
}

fn expected_previous_from_detail(detail: &str) -> i64 {
    detail
        .split(|ch: char| !ch.is_ascii_digit() && ch != '-')
        .rfind(|part| !part.is_empty())
        .and_then(|part| part.parse::<i64>().ok())
        .unwrap_or_default()
}

impl From<BackendError> for CanonicalError {
    fn from(err: BackendError) -> Self {
        match err {
            // A topic not provisioned yet, or held under another log id, is
            // retried by the caller the same way as an unreachable medium:
            // provisioning or an operator may still settle it.
            BackendError::Unavailable { .. }
            | BackendError::AppendFailed { .. }
            | BackendError::ReadFailed { .. }
            | BackendError::NotProvisioned { .. }
            | BackendError::LogConflict { .. } => CanonicalError::service_unavailable().create(),
            BackendError::InvalidConfig { detail, .. } => StorageResourceError::invalid_argument()
                .with_field_violation("backend_config", detail, reasons::INVALID_BACKEND_CONFIG)
                .create(),
            BackendError::OffsetOutOfRange {
                floor,
                ceiling,
                breached,
                detail,
                ..
            } => OffsetResourceError::out_of_range(detail.clone())
                .with_field_violation(
                    "offset",
                    format!(
                        "the position is {} the valid range [{floor}, {ceiling}]",
                        breached.phrase()
                    ),
                    breached.reason(),
                )
                .create(),
            BackendError::PartitionNotFound { detail, .. } => {
                PartitionResourceError::not_found(detail)
                    .with_resource(resources::PARTITION)
                    .create()
            }
            BackendError::Internal(detail) => CanonicalError::internal(detail).create(),
        }
    }
}

impl From<OffsetManagerError> for CanonicalError {
    fn from(err: OffsetManagerError) -> Self {
        match err {
            OffsetManagerError::InTxNotSupported { detail, .. } => {
                OffsetResourceError::failed_precondition()
                    .with_precondition_violation(
                        resources::OFFSET,
                        detail,
                        reasons::IN_TX_OFFSETS_NOT_SUPPORTED,
                    )
                    .create()
            }
            OffsetManagerError::PersistFailed { .. } | OffsetManagerError::LoadFailed { .. } => {
                CanonicalError::service_unavailable().create()
            }
            OffsetManagerError::Internal(detail) => CanonicalError::internal(detail).create(),
        }
    }
}

/// Bounded `Display` formatter for `PositionsNotSet::unseeded` - first 5 then
/// "...and N more" to keep log output bounded.
fn display_unseeded(unseeded: &[(String, u32)]) -> String {
    const CAP: usize = 5;
    let shown = unseeded
        .iter()
        .take(CAP)
        .map(|(t, p)| format!("{t}:{p}"))
        .collect::<Vec<_>>()
        .join(", ");
    if unseeded.len() > CAP {
        format!("{shown}, ...and {} more", unseeded.len() - CAP)
    } else {
        shown
    }
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum OffsetManagerError {
    #[error("offset manager does not support in-tx persistence")]
    InTxNotSupported { detail: String },

    #[error("persist failed: {reason}")]
    PersistFailed {
        reason: String,
        detail: String,
        #[source]
        source: Option<Arc<dyn std::error::Error + Send + Sync + 'static>>,
    },

    #[error("load failed: {reason}")]
    LoadFailed {
        reason: String,
        detail: String,
        #[source]
        source: Option<Arc<dyn std::error::Error + Send + Sync + 'static>>,
    },

    #[error("internal: {0}")]
    Internal(String),
}

impl OffsetManagerError {
    pub fn persist_failed(
        reason: impl Into<String>,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::PersistFailed {
            reason: reason.into(),
            detail: detail.into(),
            source: Some(Arc::new(source)),
        }
    }

    pub fn load_failed(
        reason: impl Into<String>,
        detail: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::LoadFailed {
            reason: reason.into(),
            detail: detail.into(),
            source: Some(Arc::new(source)),
        }
    }
}

/// The class of a serde_json failure (`io`, `syntax`, `data`, `eof`); serde's own
/// message can quote the value.
///
/// `io` cannot happen for in-memory input (`from_slice`, `to_value`); the arm
/// exists only because the match is exhaustive.
pub(crate) fn json_error_category(err: &serde_json::Error) -> &'static str {
    match err.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    }
}

#[cfg(test)]
mod canonical_roundtrip_tests {
    use super::*;

    /// Each of these carries identity the wire preserves, so
    /// `EventBrokerError -> CanonicalError -> EventBrokerError` returns the same
    /// variant. Variants whose payload the wire cannot carry (validation field
    /// names, counted capacities) are covered by the `From<CanonicalError>` doc,
    /// not here.
    fn recoverable_samples() -> Vec<EventBrokerError> {
        vec![
            EventBrokerError::Unauthorized {
                detail: "denied".to_owned(),
            },
            EventBrokerError::TopicNotFound {
                topic: toolkit_gts::gts_id!(
                    "cf.core.events.topic.v1~acme.billing.orders.stream.v1"
                )
                .to_owned(),
                detail: "no such topic".to_owned(),
            },
            EventBrokerError::ConsumerGroupNotFound {
                consumer_group: toolkit_gts::gts_id!(
                    "cf.core.events.consumer_group.v1~acme.billing.orders.projector.v1"
                )
                .to_owned(),
                detail: "gone".to_owned(),
            },
            EventBrokerError::SubscriptionNotFound {
                id: SubscriptionId(uuid::Uuid::from_u128(7)),
                detail: "gone".to_owned(),
            },
            EventBrokerError::UnknownProducer {
                producer: ProducerId(uuid::Uuid::from_u128(9)),
                detail: "gone".to_owned(),
            },
            EventBrokerError::EventTypeUnknown {
                event_type: toolkit_gts::gts_id!(
                    "cf.core.events.event.v1~acme.orders.order.created.v1~"
                )
                .to_owned(),
                detail: "unknown".to_owned(),
            },
            EventBrokerError::RateLimitExceeded {
                retry_after_secs: 5,
                detail: "slow down".to_owned(),
            },
            EventBrokerError::GroupAtCapacity {
                active: 3,
                partitions: 4,
                detail: "full".to_owned(),
            },
            EventBrokerError::SequenceViolation {
                expected_previous: 41,
                detail: "expected previous 41".to_owned(),
            },
            EventBrokerError::ConsumerGroupHasActiveMembers {
                detail: "busy".to_owned(),
            },
            EventBrokerError::StreamingInProgress {
                detail: "one stream".to_owned(),
            },
            EventBrokerError::InvalidProducerOptions {
                detail: "bad".to_owned(),
            },
            EventBrokerError::InvalidConsumerOptions {
                detail: "bad".to_owned(),
            },
            EventBrokerError::InvalidEventField {
                field: "source",
                detail: "must be ascii".to_owned(),
            },
            EventBrokerError::InvalidTextField {
                field: "client_agent",
                detail: "must contain only printable ASCII".to_owned(),
                reason: reasons::ASCII_ONLY,
            },
            EventBrokerError::EventDataInvalid {
                event_type: toolkit_gts::gts_id!(
                    "cf.core.events.event.v1~acme.orders.order.created.v1~"
                )
                .to_owned(),
                errors: vec!["missing field".to_owned()],
                detail: "data invalid".to_owned(),
            },
            EventBrokerError::BatchTooLarge {
                count: 200,
                bytes: 10,
                max_count: 100,
                max_bytes: 100,
                detail: "too big".to_owned(),
            },
            EventBrokerError::TypeNotInDeclaredTopic {
                event_type: toolkit_gts::gts_id!(
                    "cf.core.events.event.v1~acme.orders.order.created.v1~"
                )
                .to_owned(),
                expected_topic: toolkit_gts::gts_id!(
                    "cf.core.events.topic.v1~acme.orders.core.stream.v1"
                )
                .to_owned(),
                detail: "wrong topic".to_owned(),
            },
            EventBrokerError::SchemaNotPrepared {
                event_type: toolkit_gts::gts_id!(
                    "cf.core.events.event.v1~acme.orders.order.created.v1~"
                )
                .to_owned(),
                detail: "prepare first".to_owned(),
            },
            EventBrokerError::PositionsNotSet {
                unseeded: vec![("acme.orders.stream.v1".to_owned(), 0)],
                detail: "seek first".to_owned(),
            },
            EventBrokerError::PartitionNotAssigned {
                topic: "acme.orders.stream.v1".to_owned(),
                partition: 2,
                detail: "not yours".to_owned(),
            },
            EventBrokerError::InvalidInitialPosition {
                violations: vec![
                    PositionViolation::builder("acme.orders.stream.v1", 0)
                        .floor(Sequence::assigned(10))
                        .ceiling(Sequence::assigned(20))
                        .breached(OutOfRange::BelowFloor)
                        .build(),
                ],
                detail: "out of range".to_owned(),
            },
            EventBrokerError::Backend(BackendError::OffsetOutOfRange {
                floor: Sequence::assigned(10),
                ceiling: Sequence::assigned(20),
                breached: OutOfRange::AboveCeiling,
                detail: "offset out of range".to_owned(),
            }),
        ]
    }

    #[test]
    fn typed_errors_round_trip_through_canonical() {
        for original in recoverable_samples() {
            let canonical = CanonicalError::from(original.clone());
            let recovered = EventBrokerError::from(canonical);
            assert_eq!(
                std::mem::discriminant(&recovered),
                std::mem::discriminant(&original),
                "variant changed across the wire round-trip: {original:?} -> {recovered:?}"
            );
        }
    }

    /// A missing consumer group reads back with the `resource_name` the broker sent,
    /// unchanged: a named group's id, an anonymous group's id, or empty when the
    /// broker sent none. The canonical builder always names a resource, so the
    /// nameless case is decoded from a wire problem body the way the REST client
    /// decodes one.
    #[test]
    fn a_missing_consumer_group_reads_back_with_the_id_the_broker_sent() {
        let named = toolkit_gts::gts_id!(
            "cf.core.events.consumer_group.v1~acme.billing.orders.projector.v1"
        );
        let anonymous = format!(
            "{}00000000-0000-0000-0000-000000000abc",
            crate::gts::CONSUMER_GROUP_RESOURCE_TYPE
        );
        let cases = [
            (
                ConsumerGroupResourceError::not_found(format!(
                    "consumer group '{named}' is not registered"
                ))
                .with_resource(named.to_owned())
                .create(),
                named.to_owned(),
                format!("consumer group '{named}' is not registered"),
            ),
            (
                ConsumerGroupResourceError::not_found(format!(
                    "consumer group '{anonymous}' is not registered"
                ))
                .with_resource(anonymous.clone())
                .create(),
                anonymous.clone(),
                format!("consumer group '{anonymous}' is not registered"),
            ),
            (
                CanonicalError::try_from(
                    serde_json::from_value::<toolkit_canonical_errors::Problem>(
                        serde_json::json!({
                            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.not_found.v1~",
                            "title": "Not Found",
                            "status": 404,
                            "detail": "consumer group is not registered",
                            "context": {
                                "resource_type": "gts.cf.core.events.consumer_group.v1~",
                            },
                        }),
                    )
                    .unwrap(),
                )
                .unwrap(),
                String::new(),
                "consumer group is not registered".to_owned(),
            ),
        ];

        for (canonical, expected_group, expected_detail) in cases {
            let read_back = EventBrokerError::from(canonical);
            let EventBrokerError::ConsumerGroupNotFound {
                consumer_group,
                detail,
            } = read_back
            else {
                panic!("expected ConsumerGroupNotFound, got {read_back:?}");
            };
            assert_eq!(consumer_group, expected_group);
            assert_eq!(detail, expected_detail);
        }
    }

    #[test]
    fn sequence_violation_recovers_expected_previous() {
        let original = EventBrokerError::SequenceViolation {
            expected_previous: 41,
            detail: "expected previous 41".to_owned(),
        };
        match EventBrokerError::from(CanonicalError::from(original)) {
            EventBrokerError::SequenceViolation {
                expected_previous, ..
            } => assert_eq!(expected_previous, 41),
            other => panic!("expected SequenceViolation, got {other:?}"),
        }
    }

    /// A `client_agent` grammar refusal reads back, over REST and in process alike,
    /// as the text-field error it was, with its own reason. The field name is not
    /// recoverable (`field` is `&'static str`), and the wire `detail` is the
    /// canonical validation phrase.
    #[test]
    fn a_grammar_refusal_reads_back_as_a_text_field_error_with_its_reason() {
        let original = EventBrokerError::InvalidTextField {
            field: "client_agent",
            detail: "must follow the RFC 9110 User-Agent grammar: \
                     product *( SP ( product / comment ) )"
                .to_owned(),
            reason: reasons::USER_AGENT_GRAMMAR,
        };
        let read_back = EventBrokerError::from(CanonicalError::from(original));
        let EventBrokerError::InvalidTextField {
            field,
            detail,
            reason,
        } = read_back
        else {
            panic!("expected InvalidTextField, got {read_back:?}");
        };
        assert_eq!(field, "");
        assert_eq!(detail, "Request validation failed");
        assert_eq!(reason, "user_agent_grammar");
    }

    /// The SDK's size refusals take the broker's canonical shape (category, resource,
    /// reason, status), and read back with their sizes.
    #[test]
    fn a_size_refusal_takes_the_brokers_shape_and_reads_back_with_its_sizes() {
        let event = CanonicalError::from(EventBrokerError::EventTooLarge {
            bytes: 61_441,
            max: 61_440,
        });
        let key = CanonicalError::from(EventBrokerError::PartitionKeyTooLong {
            bytes: 1025,
            max: 1024,
        });

        assert_eq!(
            serde_json::to_value(toolkit_canonical_errors::Problem::from(event.clone())).unwrap(),
            serde_json::json!({
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
                "title": "Invalid Argument",
                "status": 413,
                "detail": "Request validation failed",
                "context": {
                    "field_violations": [{
                        "field": "event",
                        "description": "61441 bytes encoded, over the limit of 61440",
                        "reason": "event_too_large",
                    }],
                    "resource_type": toolkit_gts::gts_id!("cf.core.events.request.v1~"),
                },
            })
        );
        assert_eq!(
            serde_json::to_value(toolkit_canonical_errors::Problem::from(key.clone())).unwrap(),
            serde_json::json!({
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
                "title": "Invalid Argument",
                "status": 400,
                "detail": "Request validation failed",
                "context": {
                    "field_violations": [{
                        "field": "partition_key",
                        "description": "1025 bytes, over the limit of 1024",
                        "reason": "partition_key_too_long",
                    }],
                    "resource_type": toolkit_gts::gts_id!("cf.core.events.request.v1~"),
                },
            })
        );
        let EventBrokerError::EventTooLarge { bytes, max } = EventBrokerError::from(event) else {
            panic!("EventTooLarge must read back");
        };
        assert_eq!((bytes, max), (61_441, 61_440));
        let EventBrokerError::PartitionKeyTooLong { bytes, max } = EventBrokerError::from(key)
        else {
            panic!("PartitionKeyTooLong must read back");
        };
        assert_eq!((bytes, max), (1025, 1024));
    }

    /// The SDK's `BatchTooLarge` takes the broker's body: one `batch_too_large`
    /// violation on the bound that broke (the count first), with the broker's text,
    /// on the request resource, at 413. Read back, it gives the same body again.
    #[test]
    fn a_batch_refusal_takes_the_brokers_shape_both_ways() {
        let count = CanonicalError::from(EventBrokerError::BatchTooLarge {
            count: 101,
            bytes: 2048,
            max_count: 100,
            max_bytes: 1_048_576,
            detail: "too many".to_owned(),
        });
        let bytes = CanonicalError::from(EventBrokerError::BatchTooLarge {
            count: 3,
            bytes: 1_048_577,
            max_count: 100,
            max_bytes: 1_048_576,
            detail: "too big".to_owned(),
        });
        let body = |canonical: &CanonicalError| {
            serde_json::to_value(toolkit_canonical_errors::Problem::from(canonical.clone()))
                .unwrap()
        };
        let expected = |field: &str, description: &str| {
            serde_json::json!({
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.invalid_argument.v1~",
                "title": "Invalid Argument",
                "status": 413,
                "detail": "Request validation failed",
                "context": {
                    "field_violations": [{
                        "field": field,
                        "description": description,
                        "reason": "batch_too_large",
                    }],
                    "resource_type": toolkit_gts::gts_id!("cf.core.events.request.v1~"),
                },
            })
        };

        assert_eq!(
            body(&count),
            expected("batch.count", "batch too large: 101 events (max 100)")
        );
        assert_eq!(
            body(&bytes),
            expected(
                "batch.bytes",
                "batch payload too large: 1048577 bytes (max 1048576)"
            )
        );
        let read_back =
            [count.clone(), bytes.clone()].map(|canonical| {
                match EventBrokerError::from(canonical) {
                    EventBrokerError::BatchTooLarge {
                        count,
                        bytes,
                        max_count,
                        max_bytes,
                        ..
                    } => (count, bytes, max_count, max_bytes),
                    other => panic!("BatchTooLarge must read back, got {other:?}"),
                }
            });
        assert_eq!(read_back, [(101, 0, 100, 0), (0, 1_048_577, 0, 1_048_576)]);
        for canonical in [count, bytes] {
            let again = CanonicalError::from(EventBrokerError::from(canonical.clone()));
            assert_eq!(body(&again), body(&canonical));
        }
    }

    #[test]
    fn expected_previous_from_detail_reads_the_trailing_last_sequence() {
        assert_eq!(
            expected_previous_from_detail(
                "index=1 producer=0190f1a2-0000-7000-8000-000000000a02 \
                 topic=gts.cf.core.events.topic.v1~x.eb.chain.orders.v1 last_sequence=17"
            ),
            17
        );
    }
}

#[cfg(test)]
mod kind_tests {
    use super::*;

    #[test]
    fn kind_names_the_variant_and_none_of_its_values() {
        let data_invalid = EventBrokerError::EventDataInvalid {
            event_type: "PLANTED-VALUE-7f3a".to_owned(),
            errors: vec!["PLANTED-VALUE-7f3a".to_owned()],
            detail: "PLANTED-VALUE-7f3a".to_owned(),
        };
        assert_eq!(data_invalid.kind(), "EventDataInvalid");
        assert_eq!(
            EventBrokerError::Internal("PLANTED-VALUE-7f3a".to_owned()).kind(),
            "Internal"
        );
        assert_eq!(
            EventBrokerError::Transport("PLANTED-VALUE-7f3a".to_owned()).kind(),
            "Transport"
        );
    }

    #[test]
    fn json_error_category_names_each_reachable_class() {
        let eof = serde_json::from_slice::<serde_json::Value>(b"{").unwrap_err();
        assert_eq!(json_error_category(&eof), "eof");
        let syntax = serde_json::from_slice::<serde_json::Value>(b"nope").unwrap_err();
        assert_eq!(json_error_category(&syntax), "syntax");
        let data = serde_json::from_slice::<u32>(b"\"x\"").unwrap_err();
        assert_eq!(json_error_category(&data), "data");
    }

    #[test]
    fn decode_category_mirrors_serde_json_classes() {
        let syntax = serde_json::from_slice::<serde_json::Value>(b"nope").unwrap_err();
        let eof = serde_json::from_slice::<serde_json::Value>(b"{").unwrap_err();
        let data = serde_json::from_slice::<u32>(b"\"x\"").unwrap_err();
        let io = serde_json::Error::io(std::io::Error::other("x"));

        let categories = [syntax, eof, data, io].map(|err| {
            let category = DecodeCategory::from(err.classify());
            (category, category.to_string())
        });

        assert_eq!(
            categories,
            [
                (DecodeCategory::Syntax, "syntax".to_owned()),
                (DecodeCategory::Eof, "eof".to_owned()),
                (DecodeCategory::Data, "data".to_owned()),
                (DecodeCategory::Io, "io".to_owned()),
            ]
        );
    }

    /// The canonical form carries the type as the resource and the failure
    /// class as the violation's reason, and its text names only those two.
    #[test]
    fn a_decode_error_is_kind_decode_and_an_invalid_argument() {
        let type_id = toolkit_gts::gts_id!("cf.core.events.event.v1~acme.orders.order.created.v1~");
        let err = EventBrokerError::Decode {
            event_type: type_id.to_owned(),
            category: DecodeCategory::Data,
        };
        assert_eq!(err.kind(), "Decode");
        let display = err.to_string();

        let canonical = CanonicalError::from(err);

        let CanonicalError::InvalidArgument {
            ctx:
                toolkit_canonical_errors::context::InvalidArgumentV1::FieldViolations {
                    field_violations,
                },
            ..
        } = &canonical
        else {
            panic!("expected an InvalidArgument with field violations, got {canonical:?}");
        };
        let [violation] = field_violations.as_slice() else {
            panic!("expected one field violation, got {field_violations:?}");
        };
        assert_eq!(
            (
                violation.field.as_str(),
                violation.description.as_str(),
                violation.reason.as_str(),
                canonical.detail(),
                canonical.resource_type(),
                canonical.resource_name()
            ),
            (
                "data",
                display.as_str(),
                "decode_data",
                "Request validation failed",
                Some(toolkit_gts::gts_id!("cf.core.events.event.v1~")),
                Some(type_id)
            )
        );
        assert!(
            matches!(
                EventBrokerError::from(canonical.clone()),
                EventBrokerError::Other { .. }
            ),
            "the reverse mapping must not recover a decode error, got {:?}",
            EventBrokerError::from(canonical)
        );
        assert_eq!(
            display,
            "event data of gts.cf.core.events.event.v1~acme.orders.order.created.v1~ does not \
             decode as the payload type (data)"
        );
    }

    /// Each decode failure class has its own reason key on the canonical form.
    #[test]
    fn each_decode_category_has_its_own_canonical_reason() {
        let type_id = toolkit_gts::gts_id!("cf.core.events.event.v1~acme.orders.order.created.v1~");

        let reasons = [
            DecodeCategory::Syntax,
            DecodeCategory::Data,
            DecodeCategory::Eof,
            DecodeCategory::Io,
        ]
        .map(|category| {
            let canonical = CanonicalError::from(EventBrokerError::Decode {
                event_type: type_id.to_owned(),
                category,
            });
            let CanonicalError::InvalidArgument { ctx, .. } = &canonical else {
                panic!("expected an InvalidArgument error, got {canonical:?}");
            };
            (
                category,
                first_invalid_argument_reason(ctx).map(str::to_owned),
            )
        });

        assert_eq!(
            reasons,
            [
                (DecodeCategory::Syntax, Some("decode_syntax".to_owned())),
                (DecodeCategory::Data, Some("decode_data".to_owned())),
                (DecodeCategory::Eof, Some("decode_eof".to_owned())),
                (DecodeCategory::Io, Some("decode_io".to_owned())),
            ]
        );
    }

    /// `SequenceTaken` is SDK-local: its text names only the topic, its
    /// canonical form is an `Aborted` on that topic, and the reverse mapping
    /// does not recover it.
    #[test]
    fn a_sequence_taken_error_is_an_aborted_conflict_naming_only_its_topic() {
        let topic = toolkit_gts::gts_id!("cf.core.events.topic.v1~acme.billing.orders.stream.v1");
        let err = EventBrokerError::SequenceTaken {
            topic: topic.to_owned(),
        };

        let display = err.to_string();
        let kind = err.kind();
        let canonical = CanonicalError::from(err);

        assert_eq!(
            (display.as_str(), kind),
            (
                "the next producer sequence on topic \
                 gts.cf.core.events.topic.v1~acme.billing.orders.stream.v1 is already taken",
                "SequenceTaken"
            )
        );
        let CanonicalError::Aborted {
            ctx,
            detail,
            resource_type,
            resource_name,
            ..
        } = &canonical
        else {
            panic!("expected an Aborted error, got {canonical:?}");
        };
        assert_eq!(
            (
                ctx.reason.as_str(),
                detail.as_str(),
                resource_type.as_deref(),
                resource_name.as_deref()
            ),
            (
                "sequence_taken",
                display.as_str(),
                Some(toolkit_gts::gts_id!("cf.core.events.topic.v1~")),
                Some(topic)
            )
        );
        let recovered = EventBrokerError::from(canonical.clone());
        let EventBrokerError::Other { canonical: back } = recovered else {
            panic!("the reverse mapping must not recover a SequenceTaken, got {recovered:?}");
        };
        // `CanonicalError` has no `PartialEq`; its `Debug` output prints every field.
        assert_eq!(format!("{back:?}"), format!("{canonical:?}"));
    }

    /// `ProducerBusy` is SDK-local: its text carries only the limit, and its
    /// canonical form is a `ResourceExhausted` on the producer. The reverse
    /// mapping does not recover it: that form comes back whole as `Other`.
    #[test]
    fn a_producer_busy_error_is_resource_exhausted_naming_only_its_limit() {
        let err = EventBrokerError::ProducerBusy { limit: 64 };

        let display = err.to_string();
        let kind = err.kind();
        let canonical = CanonicalError::from(err);

        assert_eq!(
            (display.as_str(), kind),
            (
                "too many publishes wait on one topic of this producer (limit 64)",
                "ProducerBusy"
            )
        );
        let CanonicalError::ResourceExhausted {
            ctx,
            detail,
            resource_type,
            resource_name,
            ..
        } = &canonical
        else {
            panic!("expected a ResourceExhausted error, got {canonical:?}");
        };
        let [violation] = ctx.violations.as_slice() else {
            panic!("expected one quota violation, got {:?}", ctx.violations);
        };
        assert_eq!(
            (
                violation.subject.as_str(),
                violation.description.as_str(),
                violation.retry_after_seconds,
                detail.as_str(),
                resource_type.as_deref(),
                resource_name.as_deref()
            ),
            (
                "max_waiting_per_topic",
                display.as_str(),
                None,
                display.as_str(),
                Some(toolkit_gts::gts_id!("cf.core.events.producer.v1~")),
                None
            )
        );
        let recovered = EventBrokerError::from(canonical.clone());
        let EventBrokerError::Other { canonical: back } = recovered else {
            panic!("the reverse mapping must not recover a ProducerBusy, got {recovered:?}");
        };
        // `CanonicalError` has no `PartialEq`; its `Debug` output prints every field.
        assert_eq!(format!("{back:?}"), format!("{canonical:?}"));
    }

    /// Only the `ProducerBusy` form is refused: another limit on the producer
    /// resource reads back as a rate limit, with its retry hint.
    #[test]
    fn a_producer_resource_exhausted_that_is_not_busy_reads_back_as_a_rate_limit() {
        let canonical = ProducerResourceError::resource_exhausted("too many producer requests")
            .with_quota_violation("requests_per_producer", "too many producer requests")
            .with_quota_violation_retry_after_seconds(5)
            .create();

        let recovered = EventBrokerError::from(canonical);

        let EventBrokerError::RateLimitExceeded {
            retry_after_secs,
            detail,
        } = &recovered
        else {
            panic!("expected RateLimitExceeded, got {recovered:?}");
        };
        assert_eq!(
            (*retry_after_secs, detail.as_str()),
            (5, "too many producer requests")
        );
    }

    /// `OutboxQueueMismatch` is SDK-local: its text carries only the queue, and
    /// its canonical form is `Internal`, which reads back as `Internal` with the
    /// canonical internal detail, not the variant.
    #[test]
    fn an_outbox_queue_mismatch_is_internal_and_names_only_its_queue() {
        let err = EventBrokerError::OutboxQueueMismatch {
            queue: "orders-events".to_owned(),
        };

        let display = err.to_string();
        let kind = err.kind();
        let canonical = CanonicalError::from(err);

        assert_eq!(
            (display.as_str(), kind),
            (
                "the outbox the producer is bound to has no queue orders-events with its \
                 partitions",
                "OutboxQueueMismatch"
            )
        );
        let CanonicalError::Internal { ctx, detail, .. } = &canonical else {
            panic!("expected an Internal error, got {canonical:?}");
        };
        assert_eq!(
            (
                ctx.description.as_str(),
                detail.as_str(),
                canonical.resource_type()
            ),
            (
                display.as_str(),
                "An internal error occurred. Please retry later.",
                None
            )
        );
        let recovered = EventBrokerError::from(canonical);
        let EventBrokerError::Internal(detail) = &recovered else {
            panic!("expected Internal, got {recovered:?}");
        };
        assert_eq!(detail, "An internal error occurred. Please retry later.");
    }

    /// The two outbox errors of a handle from another outbox (no queue of the
    /// producer's name, or too few partitions) name only the queue.
    #[cfg(feature = "db")]
    #[test]
    fn outbox_errors_of_a_foreign_outbox_are_a_queue_mismatch() {
        use toolkit_db::outbox::OutboxError;

        for err in [
            OutboxError::QueueNotRegistered("orders-events".to_owned()),
            OutboxError::PartitionOutOfRange {
                queue: "orders-events".to_owned(),
                partition: 3,
                max: 2,
            },
        ] {
            let mapped = EventBrokerError::from(err);
            let EventBrokerError::OutboxQueueMismatch { queue } = &mapped else {
                panic!("expected OutboxQueueMismatch, got {mapped:?}");
            };
            assert_eq!(queue, "orders-events");
        }
    }

    /// A database error of the outbox is the source of an `Outbox` error by
    /// itself, so a caller walking the chain finds the `DbErr`, and the
    /// `Outbox` error's text carries the database error's.
    #[cfg(feature = "db")]
    #[test]
    fn an_outbox_database_error_keeps_the_database_error_as_its_source() {
        use std::error::Error as _;
        use toolkit_db::outbox::OutboxError;

        let mapped = EventBrokerError::from(OutboxError::Database(sea_orm::DbErr::Custom(
            "disk full".to_owned(),
        )));

        assert_eq!(
            (mapped.kind(), mapped.to_string().as_str()),
            ("Outbox", "outbox: Custom Error: disk full")
        );
        let Some(source) = mapped.source() else {
            panic!("expected a source, got {mapped:?}");
        };
        let Some(shared) = source.downcast_ref::<Arc<dyn std::error::Error + Send + Sync>>() else {
            panic!("expected a shared source, got {source:?}");
        };
        let Some(database) = shared.as_ref().downcast_ref::<sea_orm::DbErr>() else {
            panic!("expected a DbErr source, got {shared:?}");
        };
        assert!(
            matches!(database, sea_orm::DbErr::Custom(text) if text == "disk full"),
            "{database:?}"
        );
    }

    /// Every other outbox error is an `Outbox` error whose source is the
    /// outbox error itself, and whose text carries the outbox error's.
    #[cfg(feature = "db")]
    #[test]
    fn every_other_outbox_error_is_an_outbox_error_with_it_as_the_source() {
        use std::error::Error as _;
        use toolkit_db::outbox::OutboxError;

        let cases = [
            (
                OutboxError::PayloadTooLarge {
                    size: 70_000,
                    max: 65_536,
                },
                "payload size 70000 exceeds maximum 65536",
            ),
            (
                OutboxError::PartitionCountMismatch {
                    queue: "orders-events".to_owned(),
                    expected: 4,
                    found: 2,
                },
                "partition count mismatch for queue 'orders-events': expected 4, found 2",
            ),
            (
                OutboxError::EmptyTracedBatch,
                "a traced batch must carry at least one entity",
            ),
            (
                OutboxError::TracedBatchTooLarge { entities: 9 },
                "a traced batch of 9 entities exceeds the addressable maximum",
            ),
            (
                OutboxError::InvalidQueueName { reason: "empty" },
                "invalid queue name: empty",
            ),
            (
                OutboxError::InvalidPayloadType { reason: "empty" },
                "invalid payload type: empty",
            ),
            (
                OutboxError::InvalidTrace { reason: "empty" },
                "invalid trace: empty",
            ),
            (
                OutboxError::TraceTooLong {
                    size: 300,
                    max: 255,
                },
                "trace size 300 exceeds maximum 255",
            ),
            (
                OutboxError::InvalidTablePrefix { reason: "empty" },
                "invalid outbox table prefix: empty",
            ),
        ];
        for (err, expected) in cases {
            let mapped = EventBrokerError::from(err);
            assert_eq!(
                (mapped.kind(), mapped.to_string()),
                ("Outbox", format!("outbox: {expected}"))
            );
            let Some(source) = mapped.source() else {
                panic!("expected a source, got {mapped:?}");
            };
            let Some(shared) = source.downcast_ref::<Arc<dyn std::error::Error + Send + Sync>>()
            else {
                panic!("expected a shared source, got {source:?}");
            };
            let Some(outbox) = shared.as_ref().downcast_ref::<OutboxError>() else {
                panic!("expected an OutboxError source, got {shared:?}");
            };
            assert_eq!(outbox.to_string(), expected);
        }
    }

    /// An `Outbox` error is `Internal` on the wire. Its text, which a database
    /// error can fill with values, goes only into the server-side diagnostic,
    /// never into the problem body. It reads back as `Internal` with the
    /// canonical detail.
    #[cfg(feature = "db")]
    #[test]
    fn an_outbox_error_keeps_its_source_text_off_the_wire() {
        use toolkit_db::outbox::OutboxError;

        let database = sea_orm::DbErr::Custom("PLANTED-VALUE-7f3a".to_owned());
        assert!(database.to_string().contains("PLANTED-VALUE-7f3a"));

        let canonical =
            CanonicalError::from(EventBrokerError::from(OutboxError::Database(database)));

        let problem =
            serde_json::to_value(toolkit_canonical_errors::Problem::from(canonical.clone()))
                .expect("problem serializes");
        assert_eq!(
            problem,
            serde_json::json!({
                "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
                "title": "Internal",
                "status": 500,
                "detail": "An internal error occurred. Please retry later.",
                "context": {}
            })
        );
        let CanonicalError::Internal { ctx, detail, .. } = &canonical else {
            panic!("expected an Internal error, got {canonical:?}");
        };
        assert_eq!(
            (ctx.description.as_str(), detail.as_str()),
            (
                "outbox: Custom Error: PLANTED-VALUE-7f3a",
                "An internal error occurred. Please retry later."
            )
        );
        let recovered = EventBrokerError::from(canonical);
        let EventBrokerError::Internal(detail) = &recovered else {
            panic!("expected Internal, got {recovered:?}");
        };
        assert_eq!(detail, "An internal error occurred. Please retry later.");
    }

    /// The server-side diagnostic of an `Outbox` error from the outbox itself
    /// carries the outbox's text, which names queues, partitions and sizes.
    #[cfg(feature = "db")]
    #[test]
    fn an_outbox_error_diagnostic_carries_the_outbox_text() {
        use toolkit_db::outbox::OutboxError;

        let canonical =
            CanonicalError::from(EventBrokerError::from(OutboxError::PayloadTooLarge {
                size: 70_000,
                max: 65_536,
            }));

        assert_eq!(
            canonical.diagnostic(),
            Some("outbox: payload size 70000 exceeds maximum 65536")
        );
    }

    /// The server-side diagnostic of a dead-letter outbox error names the
    /// dead-letter outbox, not the producer's.
    #[cfg(feature = "db")]
    #[test]
    fn a_dead_letter_outbox_error_diagnostic_names_the_dead_letter_outbox() {
        use toolkit_db::outbox::OutboxError;

        let canonical = CanonicalError::from(EventBrokerError::outbox(
            OutboxRole::DeadLetter,
            OutboxError::PayloadTooLarge {
                size: 70_000,
                max: 65_536,
            },
        ));

        assert_eq!(
            canonical.diagnostic(),
            Some("dead-letter outbox: payload size 70000 exceeds maximum 65536")
        );
    }
}
