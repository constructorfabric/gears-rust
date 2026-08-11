//! Domain errors for the Event Broker. Canonical RFC 9457 lifting is a
//! REST-layer concern (`api/rest/error.rs`), not decided here (#4346).

use thiserror::Error;

#[derive(Debug, Error)]
pub enum DomainError {
    /// Client-supplied input failed validation (e.g.
    /// `SpecificationManager::validate_event_data`, `IngestService::publish_event`).
    /// Distinct from `Internal` so REST error mapping (#4346) can lift it to
    /// `400`/`422` instead of `500`.
    #[error("validation failed: {0}")]
    Validation(String),

    #[error("topic not found: {0}")]
    TopicNotFound(String),

    #[error("event type not found: {0}")]
    EventTypeNotFound(String),

    #[error("consumer group not found: {0}")]
    ConsumerGroupNotFound(String),

    #[error("consumer group not owned by caller: {0}")]
    ConsumerGroupNotOwned(String),

    #[error("subscription not found: {0}")]
    SubscriptionNotFound(String),

    #[error("sequence violation on topic {topic} partition {partition}")]
    SequenceViolation { topic: String, partition: i32 },

    #[error("unauthorized")]
    Unauthorized,

    #[error("storage backend unavailable: {0}")]
    StorageUnavailable(String),

    #[error("internal error: {0}")]
    Internal(String),
}
