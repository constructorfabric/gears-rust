//! Notification delivery contracts.
//!
//! Two boundaries meet here:
//!
//! - [`QuotaNotificationSinkV1`], the plugin a deployment registers to receive
//!   every event. The gear's dispatcher is its only caller.
//! - [`NotificationDeliveryV1`], the callback the gear's dispatcher gives the
//!   storage plugin. The plugin owns the one notification outbox pipeline and
//!   calls back once per claimed event; [`NotificationDeliveryHandle`] stops
//!   the pipeline again.
//!
//! Delivery is at least once. An event can reach a sink more than once, on a
//! retry or after a lease handover, so a sink is idempotent on
//! [`NotificationEvent::event_id`].

use std::time::Duration;

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use crate::models::NotificationEvent;

/// The event a sink receives: the outbox row's [`NotificationEvent`], as the
/// producing transaction enqueued it.
pub type QuotaEvent = NotificationEvent;

/// Why a sink could not take an event. Closed: the dispatcher decides retry
/// or dead-letter from the variant alone.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DispatchError {
    /// The sink did not answer in time. Retried.
    #[error("the sink timed out")]
    Timeout,
    /// A failure that may pass: the event is re-delivered later. Retried until
    /// the operator's attempt limit, then dead-lettered.
    #[error("transient sink failure: {0}")]
    Transient(String),
    /// A failure no retry can fix: the event is dead-lettered at once.
    #[error("permanent sink failure: {0}")]
    Permanent(String),
}

/// A deployment's notification sink.
///
/// The dispatcher calls every registered sink with every event, concurrently,
/// each call bounded by the operator's per-sink timeout, under the
/// dispatcher's system context. That context identifies the caller only: a
/// sink that serves one tenant filters on the event's own `scope`, never on
/// the context's tenant, and `Platform` events reach every sink.
///
/// Delivery is at least once: the same `event_id` may arrive again after a
/// retry or a lease handover, and a sink must tolerate it.
// @cpt-dod:cpt-cf-quota-enforcement-dod-sink-contract:p1
#[async_trait]
pub trait QuotaNotificationSinkV1: Send + Sync + 'static {
    /// A stable identifier, unique among a deployment's sinks; it labels the
    /// dispatch-failure telemetry.
    fn id(&self) -> &str;

    /// Take one event.
    ///
    /// # Errors
    ///
    /// [`DispatchError::Timeout`] or [`DispatchError::Transient`] to have the
    /// event re-delivered later; [`DispatchError::Permanent`] to have it
    /// dead-lettered.
    async fn dispatch(&self, ctx: &SecurityContext, event: QuotaEvent)
    -> Result<(), DispatchError>;
}

/// What the dispatcher decided for one claimed event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// Every sink took the event, or there is no sink: acknowledge it.
    Delivered,
    /// Try the event again later, for every sink.
    Retry,
    /// Dead-letter the event, with the reason.
    Reject(String),
}

/// The dispatcher as the storage plugin's outbox pipeline sees it.
#[async_trait]
pub trait NotificationDeliveryV1: Send + Sync + 'static {
    /// Deliver one claimed event.
    ///
    /// `attempts` is how many earlier deliveries of it failed (0 on the
    /// first), and `budget` how long the call may take before the pipeline's
    /// lease is at risk. A pipeline may undercount `attempts` by one, never
    /// overcount it: a limit on it bounds delivery to at most one call more.
    async fn deliver(
        &self,
        event: NotificationEvent,
        attempts: u16,
        budget: Duration,
    ) -> DeliveryOutcome;

    /// The pipeline dead-lettered a claimed message it could not decode into
    /// an event; no sink saw it. Called so the rejection is counted like any
    /// other.
    fn undeliverable(&self, payload_type: &str, reason: &str);
}

/// A running notification pipeline. Stopping it ends delivery on this
/// process; events stay queued for the next processor that claims them.
#[async_trait]
pub trait NotificationDeliveryHandle: Send + Sync {
    /// Stop the pipeline and wait for its workers to finish.
    async fn stop(self: Box<Self>);
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "notifications_tests.rs"]
mod notifications_tests;
