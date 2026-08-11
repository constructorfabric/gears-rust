//! OpenTelemetry-backed metrics for the token-issuer (DESIGN.md § 4.4).
//!
//! Instrument names are literal Prometheus form (counters end in `_total`,
//! duration histograms in `_seconds`), matching the platform's
//! `add_metric_suffixes: false` collector posture. A typed handle keeps raw
//! metric names and label wiring behind a single domain-layer port.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};
use toolkit_macros::domain_model;

/// Meter / instrumentation scope name.
pub(crate) const METER_NAME: &str = "token-issuer";

// ─── Metric names (literal Prometheus form; `add_metric_suffixes: false`) ─────
const TOKEN_ISSUER_CACHE_HITS_TOTAL: &str = "token_issuer_cache_hits_total";
const TOKEN_ISSUER_CACHE_MISSES_TOTAL: &str = "token_issuer_cache_misses_total";
const TOKEN_ISSUER_SIGN_TOTAL: &str = "token_issuer_sign_total";
const TOKEN_ISSUER_SIGN_ERRORS_TOTAL: &str = "token_issuer_sign_errors_total";
const TOKEN_ISSUER_MINT_DURATION_SECONDS: &str = "token_issuer_mint_duration_seconds";

/// OpenTelemetry-backed metrics handle for the token-issuer.
#[domain_model]
pub struct TokenIssuerMetrics {
    cache_hits_total: Counter<u64>,
    cache_misses_total: Counter<u64>,
    sign_total: Counter<u64>,
    sign_errors_total: Counter<u64>,
    mint_duration_seconds: Histogram<f64>,
}

impl std::fmt::Debug for TokenIssuerMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenIssuerMetrics").finish_non_exhaustive()
    }
}

impl TokenIssuerMetrics {
    /// Builds the instrument set from the supplied meter.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            cache_hits_total: meter
                .u64_counter(TOKEN_ISSUER_CACHE_HITS_TOTAL)
                .with_description("Capability-token cache hits (token reused)")
                .build(),
            cache_misses_total: meter
                .u64_counter(TOKEN_ISSUER_CACHE_MISSES_TOTAL)
                .with_description("Capability-token cache misses (token minted)")
                .build(),
            sign_total: meter
                .u64_counter(TOKEN_ISSUER_SIGN_TOTAL)
                .with_description("Token signing operations, by signing key class")
                .build(),
            sign_errors_total: meter
                .u64_counter(TOKEN_ISSUER_SIGN_ERRORS_TOTAL)
                .with_description("Token signing failures")
                .build(),
            mint_duration_seconds: meter
                .f64_histogram(TOKEN_ISSUER_MINT_DURATION_SECONDS)
                .with_description("End-to-end capability mint latency in seconds, by class")
                .with_boundaries(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0])
                .build(),
        }
    }

    /// Builds a handle bound to the process-global meter provider.
    #[must_use]
    pub fn from_global() -> Self {
        Self::new(&opentelemetry::global::meter(METER_NAME))
    }

    /// Records a capability-token cache hit (token reused, no signing).
    pub fn record_cache_hit(&self) {
        self.cache_hits_total.add(1, &[]);
    }

    /// Records a capability-token cache miss (a fresh token was signed).
    pub fn record_cache_miss(&self) {
        self.cache_misses_total.add(1, &[]);
    }

    /// Records one signing operation for the given key class (e.g. `cap`).
    pub fn record_sign(&self, key: &'static str) {
        self.sign_total.add(1, &[KeyValue::new("key", key)]);
    }

    /// Records one signing failure.
    pub fn record_sign_error(&self) {
        self.sign_errors_total.add(1, &[]);
    }

    /// Records one end-to-end mint duration sample for the given class.
    pub fn record_mint_duration(&self, class: &'static str, duration: Duration) {
        self.mint_duration_seconds
            .record(duration.as_secs_f64(), &[KeyValue::new("class", class)]);
    }
}
