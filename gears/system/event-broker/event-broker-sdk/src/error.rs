use std::sync::Arc;

use toolkit_canonical_errors::CanonicalError;
use toolkit_canonical_errors::resource_error;

use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};

#[resource_error(gts_id!("cf.core.event_broker.producer_options.v1~"))]
struct ProducerOptionsResourceError;
#[resource_error(gts_id!("cf.core.event_broker.consumer_options.v1~"))]
struct ConsumerOptionsResourceError;
#[resource_error(gts_id!("cf.core.event_broker.event_type.v1~"))]
struct EventTypeResourceError;
#[resource_error(gts_id!("cf.core.event_broker.topic.v1~"))]
struct TopicResourceError;
#[resource_error(gts_id!("cf.core.event_broker.consumer_group.v1~"))]
struct ConsumerGroupResourceError;
#[resource_error(gts_id!("cf.core.event_broker.subscription.v1~"))]
struct SubscriptionResourceError;
#[resource_error(gts_id!("cf.core.event_broker.producer.v1~"))]
struct ProducerResourceError;
#[resource_error(gts_id!("cf.core.event_broker.partition.v1~"))]
struct PartitionResourceError;
#[resource_error(gts_id!("cf.core.event_broker.event.v1~"))]
struct EventResourceError;
#[resource_error(gts_id!("cf.core.event_broker.stream.v1~"))]
struct StreamResourceError;
#[resource_error(gts_id!("cf.core.event_broker.storage.v1~"))]
struct StorageResourceError;
#[resource_error(gts_id!("cf.core.event_broker.offset.v1~"))]
struct OffsetResourceError;

pub mod resources {
    use toolkit_gts::gts_id;

    pub const PRODUCER_OPTIONS: &str = gts_id!("cf.core.event_broker.producer_options.v1~");
    pub const CONSUMER_OPTIONS: &str = gts_id!("cf.core.event_broker.consumer_options.v1~");
    pub const EVENT_TYPE: &str = gts_id!("cf.core.event_broker.event_type.v1~");
    pub const TOPIC: &str = gts_id!("cf.core.event_broker.topic.v1~");
    pub const CONSUMER_GROUP: &str = gts_id!("cf.core.event_broker.consumer_group.v1~");
    pub const SUBSCRIPTION: &str = gts_id!("cf.core.event_broker.subscription.v1~");
    pub const PRODUCER: &str = gts_id!("cf.core.event_broker.producer.v1~");
    pub const PARTITION: &str = gts_id!("cf.core.event_broker.partition.v1~");
    pub const EVENT: &str = gts_id!("cf.core.event_broker.event.v1~");
    pub const STREAM: &str = gts_id!("cf.core.event_broker.stream.v1~");
    pub const STORAGE: &str = gts_id!("cf.core.event_broker.storage.v1~");
    pub const OFFSET: &str = gts_id!("cf.core.event_broker.offset.v1~");
    pub const TRANSPORT: &str = gts_id!("cf.core.event_broker.transport.v1~");
}

pub mod reasons {
    pub const INVALID_PRODUCER_OPTIONS: &str = "invalid_producer_options";
    pub const INVALID_CONSUMER_OPTIONS: &str = "invalid_consumer_options";
    pub const EVENT_TYPE_NOT_DECLARED: &str = "event_type_not_declared";
    pub const INVALID_EVENT_FIELD: &str = "invalid_event_field";
    pub const EVENT_DATA_INVALID: &str = "event_data_invalid";
    pub const BATCH_TOO_LARGE: &str = "batch_too_large";
    pub const INVALID_BACKEND_CONFIG: &str = "invalid_backend_config";
    pub const TYPE_NOT_IN_DECLARED_TOPIC: &str = "type_not_in_declared_topic";
    pub const SCHEMA_NOT_PREPARED: &str = "schema_not_prepared";
    pub const CONSUMER_GROUP_HAS_ACTIVE_MEMBERS: &str = "consumer_group_has_active_members";
    pub const SEQUENCE_MISMATCH: &str = "sequence_mismatch";
    pub const POSITIONS_NOT_SET: &str = "positions_not_set";
    pub const PARTITION_NOT_ASSIGNED: &str = "partition_not_assigned";
    pub const STREAMING_IN_PROGRESS: &str = "streaming_in_progress";
    pub const IN_TX_OFFSETS_NOT_SUPPORTED: &str = "in_tx_offsets_not_supported";
    pub const RATE_LIMIT: &str = "event-broker.rate-limit";
    pub const CONSUMER_GROUP_CAPACITY: &str = "event-broker.consumer-group-capacity";
    pub const UNAUTHORIZED: &str = "event_broker_unauthorized";
    pub const STORAGE_UNAVAILABLE: &str = "storage_unavailable";
    pub const OFFSET_MANAGER_UNAVAILABLE: &str = "offset_manager_unavailable";
    pub const TRANSPORT_UNAVAILABLE: &str = "transport_unavailable";
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum EventBrokerError {
    #[error("invalid producer options: {detail}")]
    InvalidProducerOptions { detail: String, instance: String },

    #[error("invalid consumer options: {detail}")]
    InvalidConsumerOptions { detail: String, instance: String },

    #[error("event type not declared on producer: {type_id}")]
    EventTypeNotDeclared {
        type_id: String,
        detail: String,
        instance: String,
    },

    #[error("event type unknown to types-registry: {type_id}")]
    EventTypeUnknown {
        type_id: String,
        detail: String,
        instance: String,
    },

    #[error("resolved type's parent topic differs from declared topics: {type_id}")]
    TypeNotInDeclaredTopic {
        type_id: String,
        expected_topic: String,
        detail: String,
        instance: String,
    },

    #[error(
        "schema not prepared for {type_id}; call `producer.prepare::<E>(&ctx)` before opening the txn"
    )]
    SchemaNotPrepared {
        type_id: String,
        detail: String,
        instance: String,
    },

    #[error("event field invalid: {field}: {detail}")]
    InvalidEventField {
        field: &'static str,
        detail: String,
        instance: String,
    },

    #[error("event data invalid for {type_id}: {errors:?}")]
    EventDataInvalid {
        type_id: String,
        errors: Vec<String>,
        detail: String,
        instance: String,
    },

    #[error("topic not found: {topic}")]
    TopicNotFound {
        topic: String,
        detail: String,
        instance: String,
    },

    #[error("consumer group not found")]
    ConsumerGroupNotFound {
        group_id: ConsumerGroupId,
        detail: String,
        instance: String,
    },

    #[error("consumer group has active members")]
    ConsumerGroupHasActiveMembers { detail: String, instance: String },

    #[error("subscription not found: {id}")]
    SubscriptionNotFound {
        id: SubscriptionId,
        detail: String,
        instance: String,
    },

    #[error("not authorized: {detail}")]
    Unauthorized { detail: String, instance: String },

    #[error("unknown producer: {producer_id:?}")]
    UnknownProducer {
        producer_id: crate::ids::ProducerId,
        detail: String,
        instance: String,
    },

    #[error("sequence violation (broker expects previous={expected_previous})")]
    SequenceViolation {
        expected_previous: i64,
        detail: String,
        instance: String,
    },

    #[error("rate limit exceeded, retry after {retry_after_secs}s")]
    RateLimitExceeded {
        retry_after_secs: u32,
        detail: String,
        instance: String,
    },

    #[error("consumer group at capacity ({active} active members, {partitions} partitions)")]
    GroupAtCapacity {
        active: u32,
        partitions: u32,
        detail: String,
        instance: String,
    },

    #[error("publish rate limited (429), retry after {retry_after_secs}s")]
    RateLimited {
        retry_after_secs: u32,
        detail: String,
        instance: String,
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
        instance: String,
    },

    #[error("subscription recovery exhausted ({attempts} consecutive re-JOIN failures)")]
    SubscriptionRecoveryExhausted {
        attempts: u32,
        detail: String,
        instance: String,
    },

    #[error(
        "invalid initial position for {topic}:{partition}: requested {requested}, valid range \
         [retention_floor - 1, high_water_mark]"
    )]
    InvalidInitialPosition {
        topic: String,
        partition: u32,
        requested: String,
        detail: String,
        instance: String,
    },

    #[error("positions not set for {} partition(s): {}", unseeded.len(), display_unseeded(unseeded))]
    PositionsNotSet {
        unseeded: Vec<(String, u32)>,
        detail: String,
        instance: String,
    },

    #[error("partition {topic}:{partition} is not assigned to this subscription")]
    PartitionNotAssigned {
        topic: String,
        partition: u32,
        detail: String,
        instance: String,
    },

    #[error("a stream is already open for this subscription")]
    StreamingInProgress { detail: String, instance: String },

    #[error("storage backend error")]
    StorageBackend(#[from] StorageBackendError),

    #[error("offset manager error")]
    OffsetManager(#[from] OffsetManagerError),

    #[error("transport: {0}")]
    Transport(String),

    #[error("internal: {0}")]
    Internal(String),

    #[error("{canonical}")]
    Other { canonical: CanonicalError },
}

pub type ConsumerError = EventBrokerError;

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
                type_id, detail, ..
            } => EventTypeResourceError::invalid_argument()
                .with_resource(type_id)
                .with_field_violation("event_type", detail, reasons::EVENT_TYPE_NOT_DECLARED)
                .create(),
            EventBrokerError::EventTypeUnknown {
                type_id, detail, ..
            } => EventTypeResourceError::not_found(detail)
                .with_resource(type_id)
                .create(),
            EventBrokerError::TypeNotInDeclaredTopic {
                type_id,
                expected_topic,
                detail,
                ..
            } => EventTypeResourceError::failed_precondition()
                .with_resource(type_id.clone())
                .with_precondition_violation(
                    type_id,
                    format!("{detail}; expected topic {expected_topic}"),
                    reasons::TYPE_NOT_IN_DECLARED_TOPIC,
                )
                .create(),
            EventBrokerError::SchemaNotPrepared {
                type_id, detail, ..
            } => EventTypeResourceError::failed_precondition()
                .with_resource(type_id.clone())
                .with_precondition_violation(type_id, detail, reasons::SCHEMA_NOT_PREPARED)
                .create(),
            EventBrokerError::InvalidEventField { field, detail, .. } => {
                EventResourceError::invalid_argument()
                    .with_field_violation(field, detail, reasons::INVALID_EVENT_FIELD)
                    .create()
            }
            EventBrokerError::EventDataInvalid {
                type_id,
                errors,
                detail,
                ..
            } => {
                let description = if errors.is_empty() {
                    detail
                } else {
                    format!("{detail}: {}", errors.join("; "))
                };
                EventTypeResourceError::invalid_argument()
                    .with_resource(type_id)
                    .with_field_violation("data", description, reasons::EVENT_DATA_INVALID)
                    .create()
            }
            EventBrokerError::TopicNotFound { topic, detail, .. } => {
                TopicResourceError::not_found(detail)
                    .with_resource(topic)
                    .create()
            }
            EventBrokerError::ConsumerGroupNotFound {
                group_id, detail, ..
            } => ConsumerGroupResourceError::not_found(detail)
                .with_resource(group_id.to_string())
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
                producer_id,
                detail,
                ..
            } => ProducerResourceError::not_found(detail)
                .with_resource(producer_id.0.to_string())
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
            EventBrokerError::BatchTooLarge {
                count,
                bytes,
                max_count,
                max_bytes,
                detail,
                ..
            } => EventResourceError::invalid_argument()
                .with_field_violation(
                    "batch.count",
                    format!("{detail}; count={count}; max_count={max_count}"),
                    reasons::BATCH_TOO_LARGE,
                )
                .with_field_violation(
                    "batch.bytes",
                    format!("{detail}; bytes={bytes}; max_bytes={max_bytes}"),
                    reasons::BATCH_TOO_LARGE,
                )
                .create(),
            EventBrokerError::SubscriptionRecoveryExhausted { .. } => {
                CanonicalError::service_unavailable().create()
            }
            EventBrokerError::InvalidInitialPosition {
                topic,
                partition,
                requested,
                detail,
                ..
            } => PartitionResourceError::out_of_range(detail.clone())
                .with_resource(format!("{topic}:{partition}"))
                .with_field_violation(
                    "initial_position",
                    format!("{detail}; requested={requested}"),
                    "invalid_initial_position",
                )
                .create(),
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
            EventBrokerError::StreamingInProgress { detail, .. } => {
                StreamResourceError::failed_precondition()
                    .with_precondition_violation(
                        resources::STREAM,
                        detail,
                        reasons::STREAMING_IN_PROGRESS,
                    )
                    .create()
            }
            EventBrokerError::StorageBackend(err) => CanonicalError::from(err),
            EventBrokerError::OffsetManager(err) => CanonicalError::from(err),
            EventBrokerError::Transport(_) => CanonicalError::service_unavailable().create(),
            EventBrokerError::Internal(detail) => CanonicalError::internal(detail).create(),
            EventBrokerError::Other { canonical } => canonical,
        }
    }
}

impl From<CanonicalError> for EventBrokerError {
    fn from(canonical: CanonicalError) -> Self {
        let detail = canonical.detail().to_owned();
        match &canonical {
            CanonicalError::PermissionDenied { .. } => Self::Unauthorized {
                detail,
                instance: String::new(),
            },
            CanonicalError::ResourceExhausted { ctx, .. } => Self::RateLimitExceeded {
                retry_after_secs: ctx
                    .violations
                    .first()
                    .and_then(|v| v.retry_after_seconds)
                    .and_then(|v| u32::try_from(v).ok())
                    .unwrap_or_default(),
                detail,
                instance: String::new(),
            },
            CanonicalError::NotFound {
                resource_type,
                resource_name,
                ..
            } if resource_type.as_deref() == Some(resources::PRODUCER) => {
                if let Some(producer_id) = resource_name
                    .as_deref()
                    .and_then(|id| uuid::Uuid::parse_str(id).ok())
                    .map(ProducerId)
                {
                    Self::UnknownProducer {
                        producer_id,
                        detail,
                        instance: String::new(),
                    }
                } else {
                    Self::Other { canonical }
                }
            }
            CanonicalError::FailedPrecondition { ctx, .. }
                if ctx
                    .violations
                    .iter()
                    .any(|v| v.type_ == reasons::SEQUENCE_MISMATCH) =>
            {
                Self::SequenceViolation {
                    expected_previous: expected_previous_from_detail(&detail),
                    detail,
                    instance: String::new(),
                }
            }
            CanonicalError::Internal { .. } => Self::Internal(detail),
            CanonicalError::ServiceUnavailable { .. } => Self::Transport(detail),
            _ => Self::Other { canonical },
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

impl From<StorageBackendError> for CanonicalError {
    fn from(err: StorageBackendError) -> Self {
        match err {
            StorageBackendError::Unavailable { .. } => {
                CanonicalError::service_unavailable().create()
            }
            StorageBackendError::InvalidConfig { detail, .. } => {
                StorageResourceError::invalid_argument()
                    .with_field_violation("backend_config", detail, reasons::INVALID_BACKEND_CONFIG)
                    .create()
            }
            StorageBackendError::OffsetOutOfRange {
                requested,
                oldest,
                detail,
                ..
            } => OffsetResourceError::out_of_range(detail.clone())
                .with_field_violation(
                    "offset",
                    format!("{detail}; requested={requested}; oldest={oldest}"),
                    "offset_out_of_range",
                )
                .create(),
            StorageBackendError::PartitionNotFound { detail, .. } => {
                PartitionResourceError::not_found(detail)
                    .with_resource(resources::PARTITION)
                    .create()
            }
            StorageBackendError::PersistFailed { .. } | StorageBackendError::ReadFailed { .. } => {
                CanonicalError::service_unavailable().create()
            }
            StorageBackendError::Internal(detail) => CanonicalError::internal(detail).create(),
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
pub enum StorageBackendError {
    #[error("backend unavailable: {reason}")]
    Unavailable {
        reason: String,
        detail: String,
        instance: String,
    },

    #[error("invalid backend config")]
    InvalidConfig { detail: String, instance: String },

    #[error("offset out of range (requested {requested}, oldest available {oldest})")]
    OffsetOutOfRange {
        requested: i64,
        oldest: i64,
        detail: String,
        instance: String,
    },

    #[error("partition not found")]
    PartitionNotFound { detail: String, instance: String },

    #[error("persist failed: {reason}")]
    PersistFailed {
        reason: String,
        detail: String,
        instance: String,
    },

    #[error("read failed: {reason}")]
    ReadFailed {
        reason: String,
        detail: String,
        instance: String,
    },

    #[error("internal: {0}")]
    Internal(String),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum OffsetManagerError {
    #[error("offset manager does not support in-tx persistence")]
    InTxNotSupported { detail: String, instance: String },

    #[error("persist failed: {reason}")]
    PersistFailed {
        reason: String,
        detail: String,
        instance: String,
        #[source]
        source: Option<Arc<dyn std::error::Error + Send + Sync + 'static>>,
    },

    #[error("load failed: {reason}")]
    LoadFailed {
        reason: String,
        detail: String,
        instance: String,
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
            instance: String::new(),
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
            instance: String::new(),
            source: Some(Arc::new(source)),
        }
    }
}
