//! Best-effort publication of refusal events to the event broker.
//!
//! [`QueuePublisher`] is the [`EventSink`] the service emits to: a bounded
//! in-memory queue that drops (and counts) an event when full. One background
//! task ([`run`]) drains the queue and publishes each event to the broker under
//! the gate's own identity; with an absent or failing broker the event is
//! dropped, logged and counted like a queue-full drop. Event-type registration is attempted at init and retried in the
//! background while the registry is unreachable; it never blocks admissions.

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

use crate::domain::service::EventSink;
use crate::infra::metrics::AdmissionControlMetrics;

/// `source` stamped on every event this gear publishes.
pub const EVENT_SOURCE: &str = "admission-control";

/// Interval between event-type registration retries.
const REGISTRATION_RETRY: Duration = Duration::from_secs(5);

/// The bounded queue in front of the publisher task.
#[derive(Debug)]
pub struct QueuePublisher {
    tx: mpsc::Sender<RefusalEvent>,
    metrics: Arc<AdmissionControlMetrics>,
}

impl QueuePublisher {
    /// A queue of `capacity` events (at least one) and its receiving end.
    #[must_use]
    pub fn new(
        capacity: usize,
        metrics: Arc<AdmissionControlMetrics>,
    ) -> (Self, mpsc::Receiver<RefusalEvent>) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        (Self { tx, metrics }, rx)
    }
}

impl EventSink for QueuePublisher {
    fn emit(&self, event: RefusalEvent) {
        if self.tx.try_send(event).is_err() {
            self.metrics.event_dropped();
            tracing::warn!("admission-control: refusal event dropped (queue full or closed)");
        }
    }
}

/// The broker event for `event`: partitioned by the resource tenant, subject
/// the correlation identifier.
#[must_use]
pub fn to_broker_event(event: &RefusalEvent) -> Event {
    let occurred_at = DateTime::from_timestamp(
        event.occurred_at.unix_timestamp(),
        event.occurred_at.nanosecond(),
    )
    .unwrap_or(DateTime::<Utc>::UNIX_EPOCH);
    Event {
        id: Uuid::new_v4(),
        type_id: GtsTypeId::new(REFUSAL_EVENT_TYPE),
        tenant_id: event.resource_tenant_id,
        source: EVENT_SOURCE.to_owned(),
        subject: event.correlation_id.to_string(),
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
/// `Ok(false)` when the registry is unreachable (retry later); `Err` when it
/// rejected the type (a wrong deployment: startup fails).
pub async fn register_event_type(registry: &dyn TypesRegistryClient) -> anyhow::Result<bool> {
    match registry.register(vec![refusal_event_type_schema()]).await {
        Err(error) => {
            tracing::warn!(error = %error, "types-registry unreachable; refusal event type pending");
            Ok(false)
        }
        Ok(results) => {
            for result in results {
                if let RegisterResult::Err { gts_id, error } = result {
                    anyhow::bail!("types-registry rejected `{gts_id:?}`: {error}");
                }
            }
            Ok(true)
        }
    }
}

/// The publisher task: retries registration while `registered` is false, then
/// drains `rx` until `cancel` fires or the queue closes.
pub async fn run(
    mut rx: mpsc::Receiver<RefusalEvent>,
    hub: Arc<ClientHub>,
    registry: Arc<dyn TypesRegistryClient>,
    identity: SecurityContext,
    mut registered: bool,
    metrics: Arc<AdmissionControlMetrics>,
    cancel: CancellationToken,
) {
    let mut retry = tokio::time::interval(REGISTRATION_RETRY);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            _ = retry.tick(), if !registered => {
                registered = matches!(register_event_type(registry.as_ref()).await, Ok(true));
            }
            received = rx.recv() => {
                let Some(event) = received else { return };
                publish(&hub, &identity, &event, &metrics).await;
            }
        }
    }
}

async fn publish(
    hub: &ClientHub,
    identity: &SecurityContext,
    event: &RefusalEvent,
    metrics: &AdmissionControlMetrics,
) {
    let Some(broker) = hub.try_get::<dyn EventBrokerApi>() else {
        metrics.event_dropped();
        tracing::warn!("event broker unavailable; refusal event dropped");
        return;
    };
    if let Err(error) = broker.publish(identity, &to_broker_event(event)).await {
        metrics.event_dropped();
        tracing::warn!(error = %error, "refusal event publication failed; event dropped");
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "publisher_tests.rs"]
mod publisher_tests;
