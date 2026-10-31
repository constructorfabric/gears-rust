//! The activation instruments, on the process's meter.
//!
//! This build registers the key's mutation lock: `settings_mutation_lock_total`
//! by `result` and `settings_mutation_lock_wait_seconds`. No label carries a
//! key, a tenant, a value or a hash.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram};

use crate::domain::activation::LockMetrics;

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
        let meter = opentelemetry::global::meter("settings-service");
        Self {
            total: meter
                .u64_counter("settings_mutation_lock_total")
                .with_description("Mutation-lock acquisitions by result")
                .build(),
            wait: meter
                .f64_histogram("settings_mutation_lock_wait_seconds")
                .with_unit("s")
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
