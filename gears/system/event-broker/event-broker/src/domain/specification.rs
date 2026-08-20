//! `SpecificationManager` (`DESIGN.md:721-737`): shared by Ingest and
//! Delivery, owns topic/event-type metadata.

use async_trait::async_trait;
use serde_json::Value as JsonValue;
use toolkit_gts::GtsInstanceId;

use crate::domain::error::DomainError;
use crate::domain::model::{EventType, Topic};

#[async_trait]
pub trait SpecificationManager: Send + Sync {
    async fn register_topic(&self, spec: Topic) -> Result<Topic, DomainError>;
    async fn register_event_type(&self, spec: EventType) -> Result<EventType, DomainError>;
    async fn get_topic(&self, id: &GtsInstanceId) -> Option<Topic>;
    async fn get_event_type(&self, id: &GtsInstanceId) -> Option<EventType>;
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
    async fn resolve_event_type_id(&self, id: &GtsInstanceId) -> Result<i64, DomainError>;
}

/// Shared `validate_event_data` body - pure `jsonschema` validation against
/// `event_type.data_schema`, no registry/repo interaction at all. Used
/// verbatim by both `InMemoryDomainRepo` (test double) and
/// `TypesRegistrySpecificationManager` (production), so the two
/// implementations can't drift on this concern.
///
/// # Errors
/// Returns `DomainError::Internal` if `event_type.data_schema` itself is not
/// a valid JSON Schema, or `DomainError::Validation { code: "SchemaViolation",
/// .. }` if `data` fails validation against it.
pub fn validate_against_schema(
    event_type: &EventType,
    data: &JsonValue,
) -> Result<(), DomainError> {
    let validator = jsonschema::validator_for(&event_type.data_schema).map_err(|err| {
        DomainError::Internal(format!(
            "invalid data_schema on event type '{}': {err}",
            event_type.id
        ))
    })?;
    let errors: Vec<String> = validator.iter_errors(data).map(|e| e.to_string()).collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(DomainError::Validation {
            code: "SchemaViolation",
            message: errors.join("; "),
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
        gts::GtsIdPattern::try_new(pattern).map_err(|err| DomainError::Validation {
            code: "InvalidSubjectTypePattern",
            message: format!("'{pattern}' is not a valid GTS id pattern: {err}"),
        })?;
    }
    Ok(())
}
