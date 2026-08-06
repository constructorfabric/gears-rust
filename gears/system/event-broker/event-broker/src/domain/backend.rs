//! `BackendResolver` (eb-single-process-implementation D3): resolves the
//! event-storage backend for a topic. `EventRepo` no longer exists -
//! `IngestServiceImpl`/`DeliveryServiceImpl` call the resolved
//! `event_broker_sdk::EventBrokerBackend` directly for event
//! append/query/segments, with no repo-level indirection.
//!
//! Also home to the `domain::model::Event`/`Topic` <-> SDK `models::Event`
//! conversion functions every backend call site needs - this depends only
//! on `event_broker_sdk` (already a normal crate dependency, same as
//! `domain::authz`'s use of its GTS constants) and this crate's own
//! `domain::model`, so it stays within the "no infra dependencies in
//! domain" invariant despite talking about the backend boundary.

use std::sync::Arc;

use event_broker_sdk::EventBrokerBackend;
use event_broker_sdk::models::{Event as SdkEvent, ProducerMeta as SdkProducerMeta};
use toolkit_gts::GtsInstanceId;

use crate::domain::error::DomainError;
use crate::domain::model::{Event, Meta, Topic};

/// Resolves the backend for a topic. Takes `&Topic` (not nothing) so that
/// when a second backend type eventually exists, call sites - which already
/// look up the topic via `SpecificationManager` before doing anything else -
/// don't need to change; only this trait's implementation does. For this
/// change there is exactly one registered backend, so resolution is
/// trivial - deliberately not implementing DESIGN.md's full GTS
/// backend-type + `ClusterCapabilities.register_shard()` backend-instance
/// registration scheme (design.md D3's "Alternatives considered").
pub trait BackendResolver: Send + Sync {
    fn resolve(&self, topic: &Topic) -> Arc<dyn EventBrokerBackend>;
}

/// The trivial resolver for this change: always the one backend it was
/// constructed with, regardless of `topic`.
pub struct SingleBackendResolver {
    backend: Arc<dyn EventBrokerBackend>,
}

impl SingleBackendResolver {
    #[must_use]
    pub fn new(backend: Arc<dyn EventBrokerBackend>) -> Self {
        Self { backend }
    }
}

impl BackendResolver for SingleBackendResolver {
    fn resolve(&self, _topic: &Topic) -> Arc<dyn EventBrokerBackend> {
        Arc::clone(&self.backend)
    }
}

/// `domain::model::Event` (publish input) -> the SDK's `Event` shape a
/// backend's `persist` call expects. `partition` is resolved by the caller
/// (`IngestServiceImpl::publish_event`'s existing `partition_for(...)`
/// logic) before this conversion, not derived here.
#[must_use]
pub fn to_sdk_event(topic: &Topic, partition: i32, event: &Event) -> SdkEvent {
    SdkEvent {
        id: event.id,
        type_id: event.r#type.to_string(),
        topic: topic.id.to_string(),
        tenant_id: event.tenant_id,
        source: event.source.clone(),
        subject: event.subject.clone(),
        subject_type: event.subject_type.clone(),
        partition_key: event.partition_key.clone(),
        occurred_at: event.occurred_at,
        trace_parent: event.trace_parent.clone(),
        data: Some(event.data.clone()),
        // Broker-stamped fields: unset on publish input, matching the SDK's
        // own "readOnly on the wire; absent on publish" framing for these.
        partition: u32::try_from(partition).ok(),
        sequence: None,
        sequence_time: None,
        offset: None,
        offset_time: None,
        meta: event.meta.as_ref().map(to_sdk_producer_meta),
    }
}

fn to_sdk_producer_meta(meta: &Meta) -> SdkProducerMeta {
    SdkProducerMeta {
        // `Meta::version` (domain, `i32`) is always a small protocol version
        // number (`1` today) - `unwrap_or(1)` rather than propagating a
        // conversion error keeps this a pure, infallible mapping; an
        // out-of-range version would already have failed earlier
        // (schema/type validation), not here.
        version: u8::try_from(meta.version).unwrap_or(1),
        producer_id: Some(meta.producer_id),
        previous: Some(meta.previous),
        sequence: Some(meta.sequence),
        partition_hint: None,
    }
}

/// The SDK's `Event` (backend read projection) -> `domain::model::Event`.
/// `meta` is always `None` on the way back - the read projection strips
/// publish-input-only fields, matching `domain::model::Event::meta`'s own
/// doc comment ("Publish-input only; stripped on the read projection").
///
/// # Errors
/// Returns `DomainError::Internal` if `type_id`/`topic` aren't well-formed
/// GTS instance ids - would indicate backend-stored data corruption, since
/// nothing publishes an event without validating these first.
pub fn from_sdk_event(event: SdkEvent) -> Result<Event, DomainError> {
    Ok(Event {
        id: event.id,
        r#type: parse_gts_id(&event.type_id)?,
        topic: parse_gts_id(&event.topic)?,
        partition_key: event.partition_key,
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

fn parse_gts_id(raw: &str) -> Result<GtsInstanceId, DomainError> {
    GtsInstanceId::try_new(raw)
        .map_err(|e| DomainError::Internal(format!("backend returned a malformed GTS id '{raw}': {e}")))
}
