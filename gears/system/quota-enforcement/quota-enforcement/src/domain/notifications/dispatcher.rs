//! The delivery callback: fan one event out to every sink and decide its fate
//! (`features/notifications.md`, "Leased-Handler Dispatch Cycle").

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::future::join_all;
use quota_enforcement_sdk::{
    DeliveryOutcome, DispatchError, NotificationDeliveryV1, NotificationEvent,
    QuotaNotificationSinkV1,
};
use toolkit_macros::domain_model;
use toolkit_security::SecurityContext;

use crate::domain::ports::metrics::{QeMetrics, SinkLabel};

const LOG_TARGET: &str = "qe.notifications";

/// The operator's retry and timeout bounds.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchLimits {
    /// Failed deliveries after which a still-failing event is dead-lettered.
    pub max_attempts: u16,
    /// Budget for one sink to take one event.
    pub sink_timeout: Duration,
}

impl Default for DispatchLimits {
    fn default() -> Self {
        Self {
            max_attempts: 10,
            sink_timeout: Duration::from_secs(2),
        }
    }
}

/// Delivers each claimed event to every registered sink.
#[domain_model]
pub struct NotificationDispatcher {
    sinks: Vec<(SinkLabel, Arc<dyn QuotaNotificationSinkV1>)>,
    ctx: SecurityContext,
    limits: DispatchLimits,
    metrics: Arc<dyn QeMetrics>,
}

impl NotificationDispatcher {
    /// Dispatch to `sinks` under `ctx`. Bootstrap has checked the sink ids
    /// are unique.
    #[must_use]
    pub fn new(
        sinks: Vec<Arc<dyn QuotaNotificationSinkV1>>,
        ctx: SecurityContext,
        limits: DispatchLimits,
        metrics: Arc<dyn QeMetrics>,
    ) -> Self {
        let sinks = sinks
            .into_iter()
            .map(|sink| (SinkLabel::resolved(sink.id()), sink))
            .collect();
        Self {
            sinks,
            ctx,
            limits,
            metrics,
        }
    }

    /// Call one sink, bounded by `timeout`.
    async fn call(
        &self,
        sink: &dyn QuotaNotificationSinkV1,
        event: NotificationEvent,
        timeout: Duration,
    ) -> Result<(), DispatchError> {
        tokio::time::timeout(timeout, sink.dispatch(&self.ctx, event))
            .await
            .unwrap_or(Err(DispatchError::Timeout))
    }

    /// Dead-letter with `reason`, counted.
    fn reject(&self, reason: String) -> DeliveryOutcome {
        self.metrics.record_outbox_rejection();
        DeliveryOutcome::Reject(reason)
    }
}

// @cpt-algo:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1
// @cpt-flow:cpt-cf-quota-enforcement-flow-sink-delivery:p1
#[async_trait]
impl NotificationDeliveryV1 for NotificationDispatcher {
    async fn deliver(
        &self,
        event: NotificationEvent,
        attempts: u16,
        budget: Duration,
    ) -> DeliveryOutcome {
        // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-nosink-if
        if self.sinks.is_empty() {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-nosink
            return DeliveryOutcome::Delivered;
            // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-nosink
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-nosink-if

        // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-fanout
        // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-fanout
        // Every call ends before the lease does; one sink's failure or delay
        // never stops the others.
        let timeout = self.limits.sink_timeout.min(budget);
        let outcomes = join_all(self.sinks.iter().map(|(label, sink)| {
            let event = event.clone();
            async move { (label, self.call(sink.as_ref(), event, timeout).await) }
        }))
        .await;
        // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-fanout
        // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-fanout

        // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-outcome
        let mut permanent = None;
        let mut transient = None;
        for (label, outcome) in outcomes {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-ok
            let Err(error) = outcome else {
                continue;
            };
            // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-ok
            // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-transient
            // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-perm
            self.metrics
                .record_notification_dispatch_failure(label, event.kind);
            tracing::warn!(
                target: LOG_TARGET,
                sink_id = label.as_str(),
                event_id = %event.event_id,
                event_kind = event.kind.as_str(),
                attempts,
                error = %error,
                "a notification sink did not take the event"
            );
            let first = if matches!(error, DispatchError::Permanent(_)) {
                &mut permanent
            } else {
                &mut transient
            };
            first.get_or_insert((label, error));
            // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-perm
            // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-transient
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-outcome

        // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-reject-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-reject-if
        if let Some((label, error)) = permanent {
            // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-reject
            // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-dead
            return self.reject(format!(
                "sink {} refused the event: {error}",
                label.as_str()
            ));
            // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-dead
            // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-reject
        }
        if let Some((label, error)) = transient {
            if attempts >= self.limits.max_attempts {
                return self.reject(format!(
                    "delivery failed {} times; last: sink {}: {error}",
                    u32::from(attempts) + 1,
                    label.as_str()
                ));
            }
            // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-reject-if
            // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-reject-if
            // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-retry-if
            // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-retry
            // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-retry-if
            // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-retry
            return DeliveryOutcome::Retry;
            // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-retry
            // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-retry-if
            // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-retry
            // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-retry-if
        }
        // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-ack
        // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-term-if
        // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-ack
        DeliveryOutcome::Delivered
        // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-ack
        // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-term-if
        // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-ack
    }

    fn undeliverable(&self, payload_type: &str, reason: &str) {
        self.metrics.record_outbox_rejection();
        tracing::error!(
            target: LOG_TARGET,
            payload_type,
            reason,
            "a notification outbox row did not decode and was dead-lettered"
        );
    }
}
