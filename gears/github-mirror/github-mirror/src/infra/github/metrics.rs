//! OpenTelemetry instruments for the requests the mirror sends to GitHub.
//!
//! This is the gear's telemetry path for what the reference implementation
//! kept in an in-memory request vector: every request lands in a counter and
//! a latency histogram, every body in a bytes counter, and every rate-limit
//! header in a gauge, all exported by the toolkit's metrics provider.

use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram, Meter};

use super::rate_limit::RateLimitHeaders;

const METER_NAME: &str = "github-mirror";
const REQUESTS: &str = "github_mirror_github_requests_total";
const REQUEST_DURATION: &str = "github_mirror_github_request_duration_seconds";
const RESPONSE_BYTES: &str = "github_mirror_github_response_bytes_total";
const RATE_LIMIT_REMAINING: &str = "github_mirror_github_rate_limit_remaining";

/// How a GitHub request ended, the `outcome` attribute of the request counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A `2xx` with a fresh body.
    Fresh,
    /// A `304`: served from the cache, no quota spent.
    NotModified,
    /// A `429` or a rate-limit `403` that is retried.
    RateLimited,
    /// Any other status, or no response at all.
    Failed,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::NotModified => "not_modified",
            Self::RateLimited => "rate_limited",
            Self::Failed => "failed",
        }
    }
}

/// The instrument set for GitHub requests.
pub struct GithubRequestMetrics {
    requests: Counter<u64>,
    duration: Histogram<f64>,
    response_bytes: Counter<u64>,
    rate_limit_remaining: Gauge<u64>,
}

impl std::fmt::Debug for GithubRequestMetrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubRequestMetrics")
            .finish_non_exhaustive()
    }
}

impl GithubRequestMetrics {
    /// Build the instrument set from the supplied meter.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            requests: meter
                .u64_counter(REQUESTS)
                .with_description("GitHub requests sent, by method, status and outcome")
                .build(),
            duration: meter
                .f64_histogram(REQUEST_DURATION)
                .with_description(
                    "GitHub request latency in seconds, by method, status and outcome",
                )
                .with_boundaries(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0])
                .build(),
            response_bytes: meter
                .u64_counter(RESPONSE_BYTES)
                .with_description("GitHub response body bytes read, by method")
                .build(),
            rate_limit_remaining: meter
                .u64_gauge(RATE_LIMIT_REMAINING)
                .with_description("Last X-RateLimit-Remaining GitHub reported, by resource pool")
                .build(),
        }
    }

    /// Build a handle bound to the process-global meter provider.
    #[must_use]
    pub fn from_global() -> Self {
        Self::new(&opentelemetry::global::meter(METER_NAME))
    }

    /// One request that GitHub answered (or that failed before an answer, with
    /// `status` 0), with the rate-limit headers it carried.
    pub fn request(
        &self,
        method: &'static str,
        status: u16,
        outcome: Outcome,
        took: Duration,
        seen: &RateLimitHeaders,
    ) {
        let attributes = [
            KeyValue::new("method", method),
            KeyValue::new("status", i64::from(status)),
            KeyValue::new("outcome", outcome.as_str()),
        ];
        self.requests.add(1, &attributes);
        self.duration.record(took.as_secs_f64(), &attributes);
        if let Some(remaining) = seen.remaining {
            let resource = seen.resource.clone().unwrap_or_else(|| "core".to_owned());
            self.rate_limit_remaining
                .record(u64::from(remaining), &[KeyValue::new("resource", resource)]);
        }
    }

    /// A response body of `bytes` bytes read for a `method` request.
    pub fn response_bytes(&self, method: &'static str, bytes: usize) {
        self.response_bytes.add(
            u64::try_from(bytes).unwrap_or(u64::MAX),
            &[KeyValue::new("method", method)],
        );
    }
}
