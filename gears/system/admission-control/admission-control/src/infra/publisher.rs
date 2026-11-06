//! Best-effort publication of refusal events to the event broker.
//!
//! [`QueuePublisher`] is the [`EventSink`] the service emits to: a bounded
//! in-memory queue that drops (and counts) an event when full. One background
//! task ([`run`]) drains the queue and publishes each event to the broker under
//! the gate's own identity; with an absent, failing or stalled broker the event
//! is dropped, logged and counted like a queue-full drop, and so are the events
//! still queued at shutdown. The refusal event type is admitted by the runtime
//! before init. Broker calls are bounded in time and give way to cancellation.

use std::sync::Arc;
use std::time::Duration;

use admission_control_sdk::{ADMISSION_CONTROL_RESOURCE, REFUSAL_EVENT_TYPE, RefusalEvent};
use chrono::{DateTime, Utc};
use event_broker_sdk::{Event, EventBrokerApi, GtsTypeId};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::service::{EventEnvelope, EventSink};
use crate::infra::metrics::AdmissionControlMetrics;

/// `source` stamped on every event this gear publishes.
pub const EVENT_SOURCE: &str = "admission-control";

/// Bound on one broker publication.
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(5);

/// One event waiting for publication: its payload and its envelope fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedEvent {
    /// Envelope fields shared by the operation's events.
    pub envelope: EventEnvelope,
    /// The event's `data`.
    pub event: RefusalEvent,
}

/// The bounded queue in front of the publisher task.
#[derive(Debug)]
pub struct QueuePublisher {
    tx: mpsc::Sender<QueuedEvent>,
    metrics: Arc<AdmissionControlMetrics>,
}

impl QueuePublisher {
    /// A queue of `capacity` events (at least one) and its receiving end.
    #[must_use]
    pub fn new(
        capacity: usize,
        metrics: Arc<AdmissionControlMetrics>,
    ) -> (Self, mpsc::Receiver<QueuedEvent>) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        (Self { tx, metrics }, rx)
    }
}

impl EventSink for QueuePublisher {
    fn emit(&self, envelope: EventEnvelope, event: RefusalEvent) {
        if self.tx.try_send(QueuedEvent { envelope, event }).is_err() {
            self.metrics.event_dropped();
            tracing::warn!("admission-control: refusal event dropped (queue full or closed)");
        }
    }
}

/// The broker event for `queued`: tenant the resource tenant, subject the
/// correlation identifier, `data` the payload.
#[must_use]
pub fn to_broker_event(queued: &QueuedEvent) -> Event {
    let QueuedEvent { envelope, event } = queued;
    let occurred_at = DateTime::from_timestamp(
        envelope.occurred_at.unix_timestamp(),
        envelope.occurred_at.nanosecond(),
    )
    .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    Event {
        id: Uuid::new_v4(),
        type_id: GtsTypeId::new(REFUSAL_EVENT_TYPE),
        tenant_id: envelope.tenant_id,
        source: EVENT_SOURCE.to_owned(),
        subject: envelope.correlation_id.to_string(),
        subject_type: GtsTypeId::new(ADMISSION_CONTROL_RESOURCE),
        occurred_at,
        trace_parent: None,
        data: serde_json::to_value(event).ok(),
        partition: None,
        sequence: None,
        sequence_time: None,
        meta: None,
    }
}

/// Drains the admitted gear's queue until cancellation or queue closure.
/// Events still queued or in flight on cancellation are dropped and counted.
pub async fn run(
    mut rx: mpsc::Receiver<QueuedEvent>,
    hub: Arc<ClientHub>,
    identity: SecurityContext,
    metrics: Arc<AdmissionControlMetrics>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            received = rx.recv() => {
                let Some(event) = received else { return };
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        metrics.event_dropped();
                        break;
                    }
                    () = publish(&hub, &identity, &event, &metrics) => {}
                }
            }
        }
    }
    discard_queued(&mut rx, &metrics);
}

/// Drops, counts and logs the events still queued at shutdown.
fn discard_queued(rx: &mut mpsc::Receiver<QueuedEvent>, metrics: &AdmissionControlMetrics) {
    rx.close();
    let mut discarded = 0_usize;
    while rx.try_recv().is_ok() {
        metrics.event_dropped();
        discarded += 1;
    }
    if discarded > 0 {
        tracing::warn!(
            discarded,
            "admission-control stopping; queued refusal events dropped"
        );
    }
}

async fn publish(
    hub: &ClientHub,
    identity: &SecurityContext,
    event: &QueuedEvent,
    metrics: &AdmissionControlMetrics,
) {
    if let Err(reason) = deliver(hub, identity, event).await {
        metrics.event_dropped();
        tracing::warn!(%reason, "refusal event dropped");
    }
}

/// Hands `event` to the broker, or says why it could not.
async fn deliver(
    hub: &ClientHub,
    identity: &SecurityContext,
    event: &QueuedEvent,
) -> Result<(), String> {
    let broker = hub
        .try_get::<dyn EventBrokerApi>()
        .ok_or_else(|| "event broker unavailable".to_owned())?;
    let broker_event = to_broker_event(event);
    match tokio::time::timeout(PUBLISH_TIMEOUT, broker.publish(identity, &broker_event)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(format!("publication failed: {error}")),
        Err(_elapsed) => Err("event broker did not answer".to_owned()),
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "publisher_tests.rs"]
mod publisher_tests;
