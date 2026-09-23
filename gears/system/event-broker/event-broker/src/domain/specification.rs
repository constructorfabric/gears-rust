//! `SpecificationManager` (`DESIGN.md:721-737`): shared by Ingest and
//! Delivery, owns topic/event-type metadata.

use async_trait::async_trait;
use event_broker_sdk::models::EventType;
use gts::GtsTypeId;
use serde_json::Value as JsonValue;
use toolkit_gts::GtsInstanceId;

use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::model::Topic;

/// Everything the rest of the gear asks about a topic or an event type.
///
/// There is no registration path: a topic and an event type are registered by
/// the gear that owns them, at that gear's own init, and the broker only reads
/// them.
#[async_trait]
pub trait SpecificationManager: Send + Sync {
    /// The topic as the broker holds it: the projection of its registered
    /// instance, and the settings this deployment resolved for it.
    async fn get_topic(&self, id: &GtsInstanceId) -> Option<Topic>;
    /// The event type as the broker holds it, projected from its registered
    /// derived type schema. Keyed by a GTS **type** identifier.
    async fn get_event_type(&self, id: &GtsTypeId) -> Option<EventType>;
    async fn validate_event_data(
        &self,
        event_type: &EventType,
        data: &JsonValue,
    ) -> Result<(), DomainError>;

    /// `GET /v1/topics` - unfiltered/unpaginated; the REST handler applies
    /// `$filter`/pagination (`api/rest/pagination.rs`).
    async fn list_topics(&self) -> Vec<Topic>;

    /// `GET /v1/event-types` - unfiltered/unpaginated, same rationale as
    /// `list_topics`.
    async fn list_event_types(&self) -> Vec<EventType>;

    /// Resolves a topic's stable integer surrogate id
    /// (eb-single-process-implementation D1/D6) - what `Storage`'s durable
    /// tables (`cursor`, `consumer_group`) use as their foreign key into
    /// topics, instead of the much wider `GtsInstanceId` string. Stable
    /// across restarts: the same `id` is never assigned to two different
    /// `GtsInstanceId`s, and a known `GtsInstanceId` never gets a different
    /// `id` after its first resolution.
    ///
    /// # Errors
    /// Returns `DomainError::NotFound` if `id` is not a known topic.
    async fn resolve_topic_id(&self, id: &GtsInstanceId) -> Result<i64, DomainError>;

    /// Same as [`resolve_topic_id`](Self::resolve_topic_id), for event types.
    ///
    /// # Errors
    /// Returns `DomainError::NotFound` if `id` is not a known event type.
    async fn resolve_event_type_id(&self, id: &GtsTypeId) -> Result<i64, DomainError>;
}

/// Compiles `event_type.data_schema` into a reusable validator. Compilation is
/// the expensive half of validation, so the production manager compiles once and
/// caches it (`infra::specification`) rather than recompiling per published
/// event; [`check_against_schema`] then runs the cheap validation.
///
/// # Errors
/// Returns `DomainError::Internal` if `event_type.data_schema` itself is not a
/// valid JSON Schema.
pub fn compile_schema(event_type: &EventType) -> Result<jsonschema::Validator, DomainError> {
    jsonschema::validator_for(&event_type.data_schema).map_err(|err| {
        DomainError::Internal(format!(
            "invalid data_schema on event type '{}': {err}",
            event_type.id
        ))
    })
}

/// Checks `data` against an already-compiled `validator`.
///
/// # Errors
/// Returns `DomainError::Validation { code: "SchemaViolation", .. }` if `data`
/// fails validation.
pub fn check_against_schema(
    validator: &jsonschema::Validator,
    event_type: &EventType,
    data: &JsonValue,
) -> Result<(), DomainError> {
    // The jsonschema errors are not carried: they can quote the submitted
    // value, and the `(payload)` violation the REST layer emits already says
    // what is wrong without echoing request content.
    if validator.is_valid(data) {
        Ok(())
    } else {
        Err(DomainError::SchemaViolation {
            topic: event_type.topic.to_string(),
        })
    }
}

/// `EventType.allowed_subject_types` GTS-pattern validity - `SpecificationManager`'s
/// responsibility (`eb-event-type-enforcement`), checked once at registration
/// rather than defensively on every publish. Used verbatim by both
/// `InMemoryDomainRepo` and `TypesRegistrySpecificationManager` so the two
/// implementations can't drift on this concern.
///
/// # Errors
/// Returns `DomainError::Validation { code: "InvalidSubjectTypePattern", .. }`
/// if any entry in `allowed_subject_types` is not a valid `GtsIdPattern`
/// (DESIGN.md §3.1's pattern grammar: concrete Type match, `.*` wildcard
/// suffix, or bare `~` base Type).
pub fn validate_allowed_subject_types(allowed_subject_types: &[String]) -> Result<(), DomainError> {
    for pattern in allowed_subject_types {
        gts::GtsIdPattern::try_new(pattern).map_err(|_| DomainError::Validation {
            code: ErrorCode::InvalidSubjectTypePattern,
            message: "each allowed_subject_types entry must be a valid GTS id pattern".to_owned(),
        })?;
    }
    Ok(())
}
