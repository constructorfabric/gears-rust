//! The adapter's own fetch-metrics seam, shared by every source plugin.
//!
//! The fetch happens inside the source, out of the ledger's sight, so this owns
//! its metrics. A port trait keeps sources decoupled from `OTel`: unit tests use
//! [`NoopFetchMetrics`]; production wires [`OtelFetchMetrics`]. The `{provider}`
//! label is the source `provider_id`.

use std::sync::Arc;

use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};
use opentelemetry::KeyValue;

/// Records per-source fetch instruments. All methods are cheap and infallible.
pub trait FetchMetrics: Send + Sync {
    /// Fetch+parse latency (`fx_provider_fetch_duration_seconds{provider}`).
    fn observe_fetch(&self, provider: &str, seconds: f64);
    /// Pairs returned by a successful fetch (`fx_provider_rates_returned{provider}`).
    fn observe_rates_returned(&self, provider: &str, count: u64);
    /// Fetch failure by error kind (`fx_provider_fetch_errors_total{provider,kind}`).
    fn incr_fetch_error(&self, provider: &str, kind: &str);
    /// Unix time of last successful fetch (`fx_provider_last_success_timestamp{provider}`).
    fn set_last_success(&self, provider: &str, unix_secs: i64);
    /// Upstream HTTP status distribution (`fx_provider_upstream_status_total{provider,status}`).
    fn incr_upstream_status(&self, provider: &str, status: u16);
}

/// Shared handle type the sources hold.
pub type SharedFetchMetrics = Arc<dyn FetchMetrics>;

/// No-op sink: the safe default before an exporter is wired, and for unit tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopFetchMetrics;

impl FetchMetrics for NoopFetchMetrics {
    fn observe_fetch(&self, _provider: &str, _seconds: f64) {}
    fn observe_rates_returned(&self, _provider: &str, _count: u64) {}
    fn incr_fetch_error(&self, _provider: &str, _kind: &str) {}
    fn set_last_success(&self, _provider: &str, _unix_secs: i64) {}
    fn incr_upstream_status(&self, _provider: &str, _status: u16) {}
}

/// Instrumentation scope name (matches the gear/subsystem name).
const METER_NAME: &str = "bss-rate-provider";

/// `OTel`-backed fetch metrics. Instruments carry full literal Prometheus names
/// (counters end `_total`, the duration histogram `_seconds`); no `.with_unit()`,
/// matching the collector's `add_metric_suffixes: false` convention.
pub struct OtelFetchMetrics {
    fetch_duration: Histogram<f64>,
    rates_returned: Histogram<f64>,
    fetch_errors: Counter<u64>,
    last_success: Gauge<i64>,
    upstream_status: Counter<u64>,
}

impl OtelFetchMetrics {
    /// Build all instruments from a meter.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            fetch_duration: meter
                .f64_histogram("fx_provider_fetch_duration_seconds")
                .with_description("Outbound fetch+parse latency, seconds")
                .with_boundaries(vec![0.01, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0])
                .build(),
            rates_returned: meter
                .f64_histogram("fx_provider_rates_returned")
                .with_description("Pairs returned per successful fetch")
                .build(),
            fetch_errors: meter
                .u64_counter("fx_provider_fetch_errors_total")
                .with_description("Fetch failures by RateProviderError kind")
                .build(),
            last_success: meter
                .i64_gauge("fx_provider_last_success_timestamp")
                .with_description("Unix time of last successful fetch")
                .build(),
            upstream_status: meter
                .u64_counter("fx_provider_upstream_status_total")
                .with_description("Upstream HTTP status distribution")
                .build(),
        }
    }

    /// Build against the process-global meter provider (a no-op until the host
    /// wires an exporter, so this is always cheap and safe to call at `init()`).
    #[must_use]
    pub fn from_global() -> Self {
        Self::new(&opentelemetry::global::meter(METER_NAME))
    }
}

impl FetchMetrics for OtelFetchMetrics {
    fn observe_fetch(&self, provider: &str, seconds: f64) {
        self.fetch_duration
            .record(seconds, &[KeyValue::new("provider", provider.to_owned())]);
    }
    fn observe_rates_returned(&self, provider: &str, count: u64) {
        #[allow(
            clippy::cast_precision_loss,
            reason = "pair counts are tiny (tens); f64 represents them exactly"
        )]
        let count_f64 = count as f64;
        self.rates_returned
            .record(count_f64, &[KeyValue::new("provider", provider.to_owned())]);
    }
    fn incr_fetch_error(&self, provider: &str, kind: &str) {
        self.fetch_errors.add(
            1,
            &[
                KeyValue::new("provider", provider.to_owned()),
                KeyValue::new("kind", kind.to_owned()),
            ],
        );
    }
    fn set_last_success(&self, provider: &str, unix_secs: i64) {
        self.last_success
            .record(unix_secs, &[KeyValue::new("provider", provider.to_owned())]);
    }
    fn incr_upstream_status(&self, provider: &str, status: u16) {
        self.upstream_status.add(
            1,
            &[
                KeyValue::new("provider", provider.to_owned()),
                KeyValue::new("status", i64::from(status)),
            ],
        );
    }
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
