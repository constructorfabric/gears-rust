use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Meter};

use crate::domain::ports::UsersMetricsPort;

/// OpenTelemetry-backed implementation of [`UsersMetricsPort`].
pub struct UsersMetricsMeter {
    get_user_counter: Counter<u64>,
}

impl UsersMetricsMeter {
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            get_user_counter: meter
                .u64_counter("users_info.get_user")
                .with_description("Number of get_user calls")
                .build(),
        }
    }
}

impl UsersMetricsPort for UsersMetricsMeter {
    fn record_get_user(&self, result: &str) {
        self.get_user_counter
            .add(1, &[KeyValue::new("result", result.to_owned())]);
    }
}
#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
