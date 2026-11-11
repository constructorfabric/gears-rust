//! The activation instruments, on the process's meter.
//!
//! The key's mutation lock: `settings_mutation_lock_total` by `result` and
//! `settings_mutation_lock_wait_seconds`; and the notification side:
//! `settings_notify_publish_to_deliver_seconds` by `event`,
//! `settings_notify_response_latency_seconds` by `late`,
//! `settings_notify_redeliveries_total`,
//! the gauges `settings_activation_awaiting`, `settings_activation_silent`,
//! `settings_activation_silent_subscriptions` and `settings_notify_subscriptions`,
//! `settings_activation_silenced_total`, `settings_activation_incomplete_total`,
//! `settings_activation_forgotten_total`,
//! `settings_notify_reactions_total` by `outcome`,
//! `settings_notify_reports_total` by `result` and
//! `settings_activation_evictions_total` by `result`. No label carries a key, a
//! tenant, a value or a hash.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram};

/// Bucket boundaries, in seconds, of every duration histogram here. Without
/// explicit boundaries the SDK applies its millisecond-oriented defaults
/// (`0..10000`), which put every sub-5-second sample in one bucket. The tail
/// past 30 s is for the response latency, which waits on a consumer.
pub(crate) const SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 900.0,
];

use crate::domain::activation::{LockMetrics, NotifyMetrics};

/// `settings_mutation_lock_total{result}` — `acquired`, `exhausted` (the wait
/// budget ran out) or `error` (an unlock that failed, a lock session that
/// reconnected while the lock was held) — and the histogram of the time spent
/// waiting for the lock.
pub struct OtelLockMetrics {
    total: Counter<u64>,
    wait: Histogram<f64>,
}

impl OtelLockMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        Self::on(&opentelemetry::global::meter("settings-service"))
    }

    /// On `meter`.
    #[must_use]
    pub fn on(meter: &opentelemetry::metrics::Meter) -> Self {
        Self {
            total: meter
                .u64_counter("settings_mutation_lock_total")
                .with_description("Mutation-lock acquisitions by result")
                .build(),
            wait: meter
                .f64_histogram("settings_mutation_lock_wait_seconds")
                .with_unit("s")
                .with_boundaries(SECONDS_BUCKETS.to_vec())
                .with_description("Time to acquire a key's mutation lock")
                .build(),
        }
    }
}

impl Default for OtelLockMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl LockMetrics for OtelLockMetrics {
    fn acquired(&self, wait: Duration) {
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-5
        self.total.add(1, &[KeyValue::new("result", "acquired")]);
        self.wait.record(wait.as_secs_f64(), &[]);
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-5
    }

    fn exhausted(&self, wait: Duration) {
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-4
        self.total.add(1, &[KeyValue::new("result", "exhausted")]);
        self.wait.record(wait.as_secs_f64(), &[]);
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-4
    }

    fn error(&self) {
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-9
        self.total.add(1, &[KeyValue::new("result", "error")]);
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-lock:p1:inst-ca-lock-9
    }
}

/// The notification instruments, on the process's meter.
// @cpt-dod:cpt-cf-settings-service-dod-consumer-activation-observability:p1
pub struct OtelNotifyMetrics {
    publish_to_deliver: Histogram<f64>,
    response_latency: Histogram<f64>,
    redeliveries: Counter<u64>,
    awaiting: Gauge<u64>,
    subscriptions: Gauge<u64>,
    silent: Gauge<u64>,
    silent_subscriptions: Gauge<u64>,
    silenced: Counter<u64>,
    incomplete: Counter<u64>,
    forgotten: Counter<u64>,
    reactions: Counter<u64>,
    reports: Counter<u64>,
    evictions: Counter<u64>,
}

impl OtelNotifyMetrics {
    /// On the gear's meter.
    #[must_use]
    pub fn new() -> Self {
        Self::on(&opentelemetry::global::meter("settings-service"))
    }

    /// On `meter`: the thirteen instruments the notification side reports.
    #[must_use]
    pub fn on(meter: &opentelemetry::metrics::Meter) -> Self {
        Self {
            publish_to_deliver: meter
                .f64_histogram("settings_notify_publish_to_deliver_seconds")
                .with_unit("s")
                .with_boundaries(SECONDS_BUCKETS.to_vec())
                .with_description("Time from an event's occurred_at to its receipt, by event type")
                .build(),
            response_latency: meter
                .f64_histogram("settings_notify_response_latency_seconds")
                .with_unit("s")
                .with_boundaries(SECONDS_BUCKETS.to_vec())
                .with_description(
                    "Time from a change's activation state to the report that settles it, \
                     by whether the state settled was silent (late)",
                )
                .build(),
            redeliveries: meter
                .u64_counter("settings_notify_redeliveries_total")
                .with_description("change_notification events the delivery loop re-published")
                .build(),
            awaiting: meter
                .u64_gauge("settings_activation_awaiting")
                .with_description("Activation states awaiting a report")
                .build(),
            subscriptions: meter
                .u64_gauge("settings_notify_subscriptions")
                .with_description("Registered subscriptions, one per instance, key and tenant")
                .build(),
            silent: meter
                .u64_gauge("settings_activation_silent")
                .with_description("Activation states the report timeout called silent")
                .build(),
            silent_subscriptions: meter
                .u64_gauge("settings_activation_silent_subscriptions")
                .with_description("Subscriptions with silent_since set")
                .build(),
            silenced: meter
                .u64_counter("settings_activation_silenced_total")
                .with_description("Activation states the delivery loop called silent")
                .build(),
            incomplete: meter
                .u64_counter("settings_activation_incomplete_total")
                .with_description(
                    "Transitions of a subscriber's change outcome into incomplete by a call of \
                     silence",
                )
                .build(),
            forgotten: meter
                .u64_counter("settings_activation_forgotten_total")
                .with_description("Subscriptions forgotten after a day of silence")
                .build(),
            reactions: meter
                .u64_counter("settings_notify_reactions_total")
                .with_description("Reports that settled a state, by resulting status")
                .build(),
            reports: meter
                .u64_counter("settings_notify_reports_total")
                .with_description(
                    "Reports answered, by result (applied, not_subscribed, ignored_no_row, \
                     ignored_stale or ignored_repeat)",
                )
                .build(),
            evictions: meter
                .u64_counter("settings_activation_evictions_total")
                .with_description("Registration-cap decisions, by result (evicted or refused)")
                .build(),
        }
    }
}

impl Default for OtelNotifyMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl NotifyMetrics for OtelNotifyMetrics {
    fn publish_to_deliver(&self, event: &'static str, elapsed: Duration) {
        self.publish_to_deliver
            .record(elapsed.as_secs_f64(), &[KeyValue::new("event", event)]);
    }

    fn response_latency(&self, elapsed: Duration, late: bool) {
        let late = if late { "true" } else { "false" };
        self.response_latency
            .record(elapsed.as_secs_f64(), &[KeyValue::new("late", late)]);
    }

    fn redelivery(&self) {
        self.redeliveries.add(1, &[]);
    }

    fn awaiting(&self, count: u64) {
        self.awaiting.record(count, &[]);
    }

    fn subscriptions(&self, count: u64) {
        self.subscriptions.record(count, &[]);
    }

    fn silent(&self, count: u64) {
        self.silent.record(count, &[]);
    }

    fn silent_subscriptions(&self, count: u64) {
        self.silent_subscriptions.record(count, &[]);
    }

    fn silenced(&self) {
        self.silenced.add(1, &[]);
    }

    fn incomplete(&self) {
        self.incomplete.add(1, &[]);
    }

    fn forgotten(&self) {
        self.forgotten.add(1, &[]);
    }

    fn reaction(&self, outcome: &'static str) {
        self.reactions.add(1, &[KeyValue::new("outcome", outcome)]);
    }

    fn report(&self, result: &'static str) {
        self.reports.add(1, &[KeyValue::new("result", result)]);
    }

    fn eviction(&self, result: &'static str) {
        self.evictions.add(1, &[KeyValue::new("result", result)]);
    }
}

#[cfg(test)]
#[path = "activation_metrics_tests.rs"]
mod activation_metrics_tests;
