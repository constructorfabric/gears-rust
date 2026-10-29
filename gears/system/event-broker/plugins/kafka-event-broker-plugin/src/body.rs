//! The plugin's own encoding of an event in a record's value (`docs/DESIGN.md`
//! §2.2): versioned JSON of everything the record's position does not carry.

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{BackendError, Partition};
use event_broker_sdk::models::Event;
use event_broker_sdk::{GtsTypeId, Sequence};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
struct BodyV1 {
    v: u8,
    id: Uuid,
    type_id: String,
    tenant_id: Uuid,
    source: String,
    subject: String,
    subject_type: String,
    occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace_parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
}

/// `event` as a record value. Leaves out `meta` (publisher-only) and the
/// positions (the record's own).
///
/// # Errors
/// `Internal` if the payload cannot be serialized.
pub fn encode(event: &Event) -> Result<Vec<u8>, BackendError> {
    serde_json::to_vec(&BodyV1 {
        v: VERSION,
        id: event.id,
        type_id: event.type_id.as_ref().to_owned(),
        tenant_id: event.tenant_id,
        source: event.source.clone(),
        subject: event.subject.clone(),
        subject_type: event.subject_type.as_ref().to_owned(),
        occurred_at: event.occurred_at,
        trace_parent: event.trace_parent.clone(),
        data: event.data.clone(),
    })
    .map_err(|_| BackendError::Internal("an event payload could not be encoded".to_owned()))
}

/// The event a record value holds, at its position.
///
/// # Errors
/// `Internal` for a value this plugin did not write, or of a version it does
/// not know.
pub fn decode(
    body: &[u8],
    partition: Partition,
    sequence: Sequence,
    sequence_time: DateTime<Utc>,
) -> Result<Event, BackendError> {
    let stored: BodyV1 = serde_json::from_slice(body).map_err(|_| {
        BackendError::Internal("a stored event body could not be decoded".to_owned())
    })?;
    if stored.v != VERSION {
        return Err(BackendError::Internal(format!(
            "a stored event body has version {}, this plugin reads {VERSION}",
            stored.v
        )));
    }
    Ok(Event {
        id: stored.id,
        type_id: GtsTypeId::new(&stored.type_id),
        tenant_id: stored.tenant_id,
        source: stored.source,
        subject: stored.subject,
        subject_type: GtsTypeId::new(&stored.subject_type),
        occurred_at: stored.occurred_at,
        trace_parent: stored.trace_parent,
        data: stored.data,
        partition: Some(partition.value()),
        sequence: Some(sequence),
        sequence_time: Some(sequence_time),
        meta: None,
    })
}
