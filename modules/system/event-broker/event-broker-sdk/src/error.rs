use crate::ids::ConsumerGroupId;

#[derive(Debug, Clone, thiserror::Error)]
pub enum EventBrokerError {
    #[error("invalid producer options: {detail}")]
    InvalidProducerOptions { detail: String, instance: String },

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

    #[error("not authorized: {detail}")]
    Unauthorized { detail: String, instance: String },

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

    #[error("storage backend error")]
    StorageBackend(#[from] StorageBackendError),

    #[error("offset manager error")]
    OffsetManager(#[from] OffsetManagerError),

    #[error("transport: {0}")]
    Transport(String),

    #[error("internal: {0}")]
    Internal(String),
}

pub type ConsumerError = EventBrokerError;

/// Bounded `Display` formatter for `PositionsNotSet::unseeded` — first 5 then
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
    },

    #[error("load failed: {reason}")]
    LoadFailed {
        reason: String,
        detail: String,
        instance: String,
    },

    #[error("internal: {0}")]
    Internal(String),
}
