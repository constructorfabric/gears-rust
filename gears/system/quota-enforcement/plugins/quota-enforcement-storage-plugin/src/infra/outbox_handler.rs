//! The leased handler of the notification queue: the adapter between the
//! toolkit outbox and the gear dispatcher's delivery callback.
//!
//! The outbox claims a batch of one partition under a lease and hands it
//! here. Each message, in order, is decoded into its `NotificationEvent` and
//! handed to the callback with the failed attempts `ToolKit` counts for it and
//! the lease time left. `Delivered` acknowledges it, `Reject` dead-letters it
//! and moves on, `Retry` stops the batch there so the framework re-delivers
//! that message and the rest later; everything acknowledged before it stays
//! acknowledged. A message that does not decode can never be delivered, so it
//! is dead-lettered at once and reported to the callback, which counts it.
//!
//! `ToolKit` counts `attempts` per claimed batch: every message of a batch
//! claimed after its head failed carries the head's count. This handler stops
//! at the first `Retry`, so in such a batch only the head ever failed and every
//! later message is on its first delivery: the head is handed `ToolKit`'s count
//! and the rest 0.
//!
//! One failure goes uncounted. When an event first fails behind messages its
//! batch already acknowledged, `ToolKit` advances past that prefix and resets
//! the count, so the event is handed 0 once more on its next delivery; from
//! then on it heads its partition and every failure counts. An event's
//! `attempts` is therefore its own failed deliveries, or one fewer.

use std::sync::Arc;

use async_trait::async_trait;
use quota_enforcement_sdk::{DeliveryOutcome, NotificationDeliveryV1, NotificationEvent};
use toolkit_db::outbox::{Batch, HandlerResult, LeasedHandler};

/// Dispatches claimed notification events to the delivery callback.
pub(super) struct NotificationHandler {
    delivery: Arc<dyn NotificationDeliveryV1>,
}

impl NotificationHandler {
    pub(super) fn new(delivery: Arc<dyn NotificationDeliveryV1>) -> Self {
        Self { delivery }
    }
}

// @cpt-algo:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1
// @cpt-state:cpt-cf-quota-enforcement-state-outbox-event:p1
// @cpt-flow:cpt-cf-quota-enforcement-flow-sink-delivery:p1
#[async_trait]
impl LeasedHandler for NotificationHandler {
    async fn handle(&self, batch: &mut Batch<'_>) -> HandlerResult {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-claim
        // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-claim
        // @cpt-begin:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-each
        let mut head = true;
        loop {
            // The lease time left, read before `next_msg` borrows the batch.
            let budget = batch.remaining();
            if budget.is_zero() {
                // The rest is re-delivered after a backoff; a partial success
                // would leave it waiting for the next wake instead.
                return HandlerResult::Retry {
                    reason: "the notification lease ran out".to_owned(),
                };
            }
            let Some(message) = batch.next_msg() else {
                break;
            };
            let decoded = serde_json::from_slice::<NotificationEvent>(&message.payload);
            let payload_type = message.payload_type.clone();
            // ToolKit counts from 0 and never goes negative; a negative value
            // would read as a first attempt rather than wrap.
            let attempts = if head {
                u16::try_from(message.attempts).unwrap_or(0)
            } else {
                0
            };
            head = false;
            let event = match decoded {
                Ok(event) => event,
                Err(error) => {
                    let reason = format!("undecodable notification event: {error}");
                    self.delivery.undeliverable(&payload_type, &reason);
                    batch.reject(reason);
                    continue;
                }
            };
            match self.delivery.deliver(event, attempts, budget).await {
                // @cpt-begin:cpt-cf-quota-enforcement-state-outbox-event:p1:inst-obst-delivered
                DeliveryOutcome::Delivered => batch.ack(),
                // @cpt-end:cpt-cf-quota-enforcement-state-outbox-event:p1:inst-obst-delivered
                // @cpt-begin:cpt-cf-quota-enforcement-state-outbox-event:p1:inst-obst-dead
                DeliveryOutcome::Reject(reason) => batch.reject(reason),
                // @cpt-end:cpt-cf-quota-enforcement-state-outbox-event:p1:inst-obst-dead
                // @cpt-begin:cpt-cf-quota-enforcement-state-outbox-event:p1:inst-obst-retry
                DeliveryOutcome::Retry => {
                    return HandlerResult::Retry {
                        reason: "a notification sink asked for redelivery".to_owned(),
                    };
                } // @cpt-end:cpt-cf-quota-enforcement-state-outbox-event:p1:inst-obst-retry
            }
        }
        // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-each
        // @cpt-end:cpt-cf-quota-enforcement-algo-dispatcher-singleton:p1:inst-disp-claim
        // @cpt-end:cpt-cf-quota-enforcement-flow-sink-delivery:p1:inst-del-claim
        HandlerResult::Success
    }
}
