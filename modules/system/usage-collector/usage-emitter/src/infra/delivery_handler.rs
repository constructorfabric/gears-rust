use std::sync::Arc;

use async_trait::async_trait;
use modkit_db::outbox::{LeasedMessageHandler, MessageResult, OutboxMessage};
use tracing::{debug, error, info, warn};
use usage_collector_sdk::UsageCollectorClientV1;
use usage_collector_sdk::UsageCollectorError;
use usage_collector_sdk::models::UsageRecord;

/// Outbox delivery handler that forwards dequeued usage records to the usage collector,
/// calling [`UsageCollectorClientV1::create_usage_record`] once per message.
///
/// Implements [`LeasedMessageHandler`]:
/// - Deserialization failures dead-letter the message (permanent: corrupt payload).
/// - Transient collector errors trigger retry: [`UsageCollectorError::DeadlineExceeded`],
///   [`UsageCollectorError::ResourceExhausted`], and
///   [`UsageCollectorError::ServiceUnavailable`] (timeout, rate limiting, transport
///   failure, or server-side condition that is expected to recover). The retry set is
///   restricted to canonically-retryable gRPC variants.
/// - `Cancelled` and `Aborted` are also retried (concurrency conflict / client
///   cancellation are recoverable on a later attempt).
/// - Caller-induced collector errors dead-letter the message (permanent): `InvalidArgument`,
///   `Unauthenticated`, `PermissionDenied`, `NotFound`, `AlreadyExists`,
///   `FailedPrecondition`, `OutOfRange`, `Unimplemented`. Retrying these would loop forever.
/// - `Internal`, `Unknown`, and `DataLoss` dead-letter the message (permanent). Per gRPC
///   canonical semantics they signal serious defects, an unrecognized error space, or
///   unrecoverable corruption — none of which improve with retry. Real transient infra
///   conditions (plugin not ready, types registry down, circuit breaker open, transport
///   errors) surface as `ServiceUnavailable` instead (see `domain/error.rs`). Dead-lettering
///   preserves the message for operator inspection rather than burning the retry budget on
///   unfixable errors.
pub struct DeliveryHandler {
    collector: Arc<dyn UsageCollectorClientV1>,
}

impl DeliveryHandler {
    #[must_use]
    pub fn new(collector: Arc<dyn UsageCollectorClientV1>) -> Self {
        Self { collector }
    }
}

#[async_trait]
impl LeasedMessageHandler for DeliveryHandler {
    // @cpt-algo:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1
    // @cpt-flow:cpt-cf-usage-collector-flow-sdk-and-ingest-core-emit:p1
    async fn handle(&self, msg: &OutboxMessage) -> MessageResult {
        // Processor may pass several messages per lease cycle (see WorkerTuning::batch_size);
        // this handler still delivers each payload individually.

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-1
        let record = match serde_json::from_slice::<UsageRecord>(&msg.payload) {
            Ok(r) => r,
            // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-2
            Err(err) => {
                warn!(
                    msg.seq,
                    msg.partition_id,
                    %err,
                    "usage record deserialization failed"
                );
                // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-2a
                return MessageResult::Reject(format!("{:#}", anyhow::Error::new(err)));
                // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-2a
            } // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-2
        };
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-1

        // inst-dlv-3: gateway ingest request assembly from UsageRecord fields is performed
        // inside UsageCollectorClientV1::create_usage_record — see rest_client.rs
        // (UsageRecord IS the request at this layer; DTO assembly is an implementation detail
        //  of the REST client adapter).

        // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-4
        let delivery_result = self.collector.create_usage_record(record).await;
        // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-4

        match delivery_result {
            // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-5
            Ok(()) => {
                debug!("usage record delivered to collector");
                // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-5a
                MessageResult::Ok
                // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-5a
            }
            // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-5
            // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-6
            Err(
                e @ (UsageCollectorError::DeadlineExceeded { .. }
                | UsageCollectorError::ResourceExhausted { .. }
                | UsageCollectorError::ServiceUnavailable { .. }
                | UsageCollectorError::Cancelled { .. }
                | UsageCollectorError::Aborted { .. }),
            ) => {
                // Retry set is restricted to canonically-retryable gRPC variants;
                // see the type-level doc comment for the full rationale.
                info!(error = %e, "transient collector delivery error; will retry");
                // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-6a
                MessageResult::Retry
                // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-6a
            }
            // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-6
            // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-7
            Err(e) => {
                error!(error = %e, "permanent collector delivery error; dead-lettering message");
                // @cpt-begin:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-7a
                MessageResult::Reject(format!("{:#}", anyhow::Error::new(e)))
                // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-7a
            } // @cpt-end:cpt-cf-usage-collector-algo-sdk-and-ingest-core-outbox-delivery:p1:inst-dlv-7
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "delivery_handler_tests.rs"]
mod delivery_handler_tests;
