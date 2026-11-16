//! The plugin's own encoding of an event in `body` (`docs/DESIGN.md` §2.3):
//! versioned JSON of everything a column does not carry.

use chrono::{DateTime, Utc};
use event_broker_sdk::backend::{BackendError, Partition};
use event_broker_sdk::models::{Event, IngestStamp};
use event_broker_sdk::{GtsTypeId, Sequence};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
struct BodyV1 {
    v: u8,
    id: Uuid,
    type_id: String,
    subject_tenant: Uuid,
    source: String,
    subject: String,
    subject_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    subject_owner: Option<Uuid>,
    occurred_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace_parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    data: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meta: Option<BodyMeta>,
}

/// The broker's internal ingest stamp, under the body's `meta`. Never the
/// producer's chain metadata.
#[derive(Serialize, Deserialize)]
struct BodyMeta {
    ingest_tenant: Uuid,
    ingest_actor: Uuid,
}

/// `event` as `body` stores it. Leaves out the producer's `meta` (publisher-only)
/// and the positions (columns); keeps the ingest stamp under `meta`.
///
/// # Errors
/// `Internal` if the payload cannot be serialized.
pub fn encode(event: &Event) -> Result<Vec<u8>, BackendError> {
    serde_json::to_vec(&BodyV1 {
        v: VERSION,
        id: event.id,
        type_id: event.event_type.as_ref().to_owned(),
        subject_tenant: event.subject_tenant,
        source: event.source.clone(),
        subject: event.subject.clone(),
        subject_type: event.subject_type.as_ref().to_owned(),
        subject_owner: event.subject_owner,
        occurred_at: event.occurred_at,
        trace_parent: event.trace_parent.clone(),
        data: event.data.clone(),
        meta: event.ingest.map(|stamp| BodyMeta {
            ingest_tenant: stamp.tenant,
            ingest_actor: stamp.actor,
        }),
    })
    .map_err(|_| BackendError::Internal("an event payload could not be encoded".to_owned()))
}

/// The event `body` stores, at its position.
///
/// # Errors
/// `Internal` for a body this plugin did not write, or of a version it does
/// not know.
pub fn decode(
    body: &[u8],
    partition: Partition,
    sequence: i64,
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
        event_type: GtsTypeId::new(&stored.type_id),
        subject_tenant: stored.subject_tenant,
        source: stored.source,
        subject: stored.subject,
        subject_type: GtsTypeId::new(&stored.subject_type),
        subject_owner: stored.subject_owner,
        occurred_at: stored.occurred_at,
        trace_parent: stored.trace_parent,
        data: stored.data,
        partition: Some(partition.value()),
        sequence: Some(Sequence::assigned(sequence)),
        sequence_time: Some(sequence_time),
        meta: None,
        ingest: stored.meta.map(|meta| IngestStamp {
            tenant: meta.ingest_tenant,
            actor: meta.ingest_actor,
        }),
    })
}
