//! The `domain::model::Event`/`Topic` <-> SDK `models::Event` conversion
//! functions every backend call site needs - this depends only on
//! `event_broker_sdk` (already a normal crate dependency, same as
//! `domain::authz`'s use of its GTS constants) and this crate's own
//! `domain::model`, so it stays within the "no infra dependencies in
//! domain" invariant despite talking about the backend boundary.

use event_broker_sdk::models::Event as SdkEvent;
use toolkit_gts::GtsInstanceId;

use crate::domain::error::{DomainError, ErrorCode};
use crate::domain::ingest::PublishRequest;
use crate::domain::model::Event;

/// The value an event type's partition-key pointer resolves to within a publish
/// request, as the string whose bytes are hashed.
///
/// Delegates to the SDK's own [`SdkEvent::partition_input`], which owns the
/// mapping from schema member names to fields: a pointer addresses `/tenant_id`
/// or `/data/order_id`, not a Rust identifier, and a second copy of that
/// mapping here is exactly the kind of thing that drifts. A producer computing
/// a local hint resolves the pointer through the same function.
///
/// # Errors
/// [`DomainError::Validation`] when the pointer resolves to nothing, to null,
/// or to an object or array. The registration check proves the member is
/// *declared*, not that every event carries it, so an event omitting an
/// optional member is refused here rather than silently routed somewhere else.
pub fn partition_input(request: &PublishRequest, pointer: &str) -> Result<String, DomainError> {
    let addressable = SdkEvent {
        id: request.id,
        type_id: request.r#type.clone(),
        tenant_id: request.tenant_id,
        source: request.source.clone(),
        subject: request.subject.clone(),
        subject_type: request.subject_type.clone(),
        occurred_at: request.occurred_at,
        trace_parent: request.trace_parent.clone(),
        data: Some(request.data.clone()),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: None,
    };
    addressable
        .partition_input(pointer)
        .map_err(|err| DomainError::Validation {
            code: ErrorCode::PartitionKeyUnresolved,
            message: format!(
                "event type '{}' partitions on `{pointer}`, which this event does not resolve: \
                 {err}",
                request.r#type.as_ref()
            ),
        })
}

/// `domain::model::Event` (publish input) -> the SDK's `Event` a backend's
/// `append` stores. Carries no partition, no sequence and no producer
/// metadata: the first two are the backend's to assign, and the producer
/// chain was checked and recorded at admission, so nothing downstream reads
/// it.
///
/// The owning topic is not part of the SDK event: `append` takes it as its
/// own argument, so the caller names the log it is appending to rather than
/// repeating it once per event.
#[must_use]
pub fn to_sdk_event(event: &Event) -> SdkEvent {
    SdkEvent {
        id: event.id,
        type_id: event.r#type.clone(),
        tenant_id: event.tenant_id,
        source: event.source.clone(),
        subject: event.subject.clone(),
        subject_type: event.subject_type.clone(),
        occurred_at: event.occurred_at,
        trace_parent: event.trace_parent.clone(),
        data: Some(event.data.clone()),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: None,
    }
}

/// The SDK's `Event` (backend read projection) -> `domain::model::Event`.
/// `meta` is always `None` on the way back - the read projection strips
/// publish-input-only fields, matching `domain::model::Event::meta`'s own
/// doc comment ("Publish-input only; stripped on the read projection").
///
/// `topic` comes from the caller, which already knows the log it read: the
/// SDK event carries no topic, and re-deriving one per event from its type
/// would ask the specification manager the same question the read itself
/// already answered.
///
/// # Errors
/// Returns `DomainError::Internal` if `type_id` isn't a well-formed GTS
/// instance id - would indicate backend-stored data corruption, since
/// nothing publishes an event without validating it first.
pub fn from_sdk_event(topic: &GtsInstanceId, event: SdkEvent) -> Result<Event, DomainError> {
    Ok(Event {
        id: event.id,
        r#type: parse_gts_type_id(&event.type_id)?,
        topic: topic.clone(),
        tenant_id: event.tenant_id,
        source: event.source,
        subject: event.subject,
        subject_type: event.subject_type,
        occurred_at: event.occurred_at,
        trace_parent: event.trace_parent,
        data: event.data.unwrap_or(serde_json::Value::Null),
        meta: None,
        partition: event.partition.and_then(|p| i32::try_from(p).ok()),
        sequence: event.sequence,
        sequence_time: event.sequence_time,
    })
}

/// The same, for the event's own type: a backend stores the identifier as a
/// string, and an event type is a GTS type rather than an instance of one.
fn parse_gts_type_id(raw: &str) -> Result<gts::GtsTypeId, DomainError> {
    gts::GtsTypeId::try_new(raw).map_err(|e| {
        DomainError::Internal(format!(
            "backend returned a malformed GTS type id '{raw}': {e}"
        ))
    })
}
