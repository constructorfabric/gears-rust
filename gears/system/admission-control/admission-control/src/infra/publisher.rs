//! Best-effort publication of refusal events to the event broker.
//!
//! [`QueuePublisher`] is the [`EventSink`] the service emits to: a bounded
//! in-memory queue that drops (and counts) an event when full. One background
//! task ([`run`]) drains the queue and publishes each event to the broker under
//! the gate's own identity; with an absent, failing or stalled broker the event
//! is dropped, logged and counted like a queue-full drop, and so are the events
//! still queued at shutdown. Event-type registration is attempted at init and
//! retried in the background while the registry is unreachable or does not
//! answer; it never blocks admissions. Every registry and broker call is
//! bounded in time and gives way to cancellation.

use std::sync::Arc;
use std::time::Duration;

use admission_control_sdk::gts::refusal_event_type_schema;
use admission_control_sdk::{ADMISSION_CONTROL_RESOURCE, REFUSAL_EVENT_TYPE, RefusalEvent};
use chrono::{DateTime, Utc};
use event_broker_sdk::{Event, EventBrokerApi, GtsTypeId};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use toolkit::ClientHub;
use toolkit_security::SecurityContext;
use types_registry_sdk::{RegisterResult, TypesRegistryClient};
use uuid::Uuid;

use crate::domain::service::{EventEnvelope, EventSink};
use crate::infra::metrics::AdmissionControlMetrics;

/// `source` stamped on every event this gear publishes.
pub const EVENT_SOURCE: &str = "admission-control";

/// Interval between event-type registration retries.
const REGISTRATION_RETRY: Duration = Duration::from_secs(5);

/// Bound on one event-type registration call.
const REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Registers the refusal event type.
///
/// # Errors
///
/// `Ok(false)` when the registry is unreachable or does not answer within
/// [`REGISTRATION_TIMEOUT`] (retry later); `Err` when it rejected the type (a
/// wrong deployment: startup fails).
///
/// The call is bounded although today's in-process client answers without
/// suspending: the client comes from `ClientHub`, and a remote one would
/// replace it with no change here.
pub async fn register_event_type(registry: &dyn TypesRegistryClient) -> anyhow::Result<bool> {
    let registration = registry.register(vec![refusal_event_type_schema()]);
    match tokio::time::timeout(REGISTRATION_TIMEOUT, registration).await {
        Err(_elapsed) => {
            tracing::warn!("types-registry did not answer; refusal event type pending");
            Ok(false)
        }
        Ok(Err(error)) => {
            tracing::warn!(error = %error, "types-registry unreachable; refusal event type pending");
            Ok(false)
        }
        Ok(Ok(results)) => {
            for result in results {
                if let RegisterResult::Err { gts_id, error } = result {
                    anyhow::bail!("types-registry rejected `{gts_id:?}`: {error}");
                }
            }
            Ok(true)
        }
    }
}

/// The publisher task: retries registration while `registered` is false
/// (until it succeeds or the registry rejects the type), then drains `rx`
/// until `cancel` fires or the queue closes. Events still queued, or in
/// flight, when `cancel` fires are dropped and counted.
pub async fn run(
    mut rx: mpsc::Receiver<QueuedEvent>,
    hub: Arc<ClientHub>,
    registry: Arc<dyn TypesRegistryClient>,
    identity: SecurityContext,
    registered: bool,
    metrics: Arc<AdmissionControlMetrics>,
    cancel: CancellationToken,
) {
    let mut retrying = !registered;
    let mut retry = tokio::time::interval(REGISTRATION_RETRY);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            _ = retry.tick(), if retrying => {
                let outcome = tokio::select! {
                    biased;
                    () = cancel.cancelled() => break,
                    outcome = register_event_type(registry.as_ref()) => outcome,
                };
                retrying = still_pending(outcome);
            }
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

/// Whether registration must be retried after `outcome`: only while the
/// registry is unreachable.
fn still_pending(outcome: anyhow::Result<bool>) -> bool {
    match outcome {
        Ok(registered) => !registered,
        Err(error) => {
            // Startup fails on the same rejection; here the gate is already
            // serving, so stop retrying and say so.
            tracing::error!(
                error = %error,
                "types-registry rejected the refusal event type; not retrying"
            );
            false
        }
    }
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
