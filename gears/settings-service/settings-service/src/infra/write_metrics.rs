// Created: 2026-09-07 by Constructor Tech
// @cpt-dod:cpt-cf-settings-service-dod-value-writes-observability:p1
//! The write path's counters, and the Change Publisher binding of this
//! release.

use async_trait::async_trait;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Counter;
use tracing::info;

use crate::domain::ports::{ChangePublisher, ValueEvent, WriteMetrics};

/// `settings_value_writes_total` and `settings_step_up_total` on the
/// process's meter. The failure ratio is a dashboard derivation of the first.
pub struct OtelWriteMetrics {
    writes: Counter<u64>,
    step_ups: Counter<u64>,
}

impl OtelWriteMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("settings-service");
        Self {
            writes: meter
                .u64_counter("settings_value_writes_total")
                .with_description("Value writes by result")
                .build(),
            step_ups: meter
                .u64_counter("settings_step_up_total")
                .with_description("Step-up verifications by operation and result")
                .build(),
        }
    }
}

impl Default for OtelWriteMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteMetrics for OtelWriteMetrics {
    fn value_write(&self, result: &'static str) {
        self.writes.add(1, &[KeyValue::new("result", result)]);
    }

    fn step_up(&self, operation: &'static str, result: &'static str) {
        self.step_ups.add(
            1,
            &[
                KeyValue::new("operation", operation),
                KeyValue::new("result", result),
            ],
        );
    }
}

/// The Change Publisher of a release with no broker bound: the event is
/// logged, so a failed change is still a visible notification, and the write
/// path's contract — commit, evict, publish — is exercised end to end.
pub struct LoggingPublisher;

#[async_trait]
impl ChangePublisher for LoggingPublisher {
    async fn publish(&self, event: ValueEvent) {
        match event {
            ValueEvent::Changed {
                key,
                tenant_id,
                actor,
                change_set_id,
            } => info!(
                event = "event_value_changed",
                %key,
                %tenant_id,
                %actor,
                %change_set_id,
                "setting value changed"
            ),
            ValueEvent::ChangeFailed {
                key,
                tenant_id,
                actor,
                reason,
            } => info!(
                event = "event_value_change_failed",
                %key,
                %tenant_id,
                %actor,
                %reason,
                "setting value change rejected"
            ),
        }
    }
}
