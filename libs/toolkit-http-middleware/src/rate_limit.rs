//! Per-route rate limiting and in-flight limiting middleware.
//!
//! The [`RateLimiterMap`] (`(method, path)` → token bucket + in-flight
//! semaphore) is built by the consuming gear from its operation specs and
//! configuration; this crate owns the runtime type and the request-time
//! middleware. The rate-limit rejection is rendered under a caller-supplied GTS
//! `scope`.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use axum::http::{HeaderValue, Method, header};
use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use governor::clock::Clock;
use governor::middleware::StateInformationMiddleware;
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use toolkit_canonical_errors::CanonicalError;

use crate::common;

/// Deserializable rate-limit parameters (requests/sec, burst capacity, and
/// max concurrent in-flight requests). Consumers typically use this as the
/// per-gear fallback applied to routes that declare no explicit limit.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct RateLimitConfig {
    /// Sustained requests per second.
    pub rps: u32,
    /// Maximum burst capacity.
    pub burst: u32,
    /// Maximum concurrent in-flight requests.
    pub in_flight: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            rps: 50,
            burst: 100,
            in_flight: 64,
        }
    }
}

type RateLimitKey = (Method, String);
type BucketMap = Arc<HashMap<RateLimitKey, Arc<BucketMapEntry>>>;
type InflightMap = Arc<HashMap<RateLimitKey, Arc<Semaphore>>>;

/// Per-route rate-limit + in-flight lookup, plus the GTS `scope` under which
/// rejections are rendered.
#[derive(Default, Clone)]
pub struct RateLimiterMap {
    buckets: BucketMap,
    inflight: InflightMap,
    scope: &'static str,
}

struct BucketMapEntry {
    bucket: DefaultDirectRateLimiter<StateInformationMiddleware>,
    policy: HeaderValue,
    burst: HeaderValue,
}

impl BucketMapEntry {
    fn new(rps: u32, burst: u32) -> Result<Self> {
        let bucket = RateLimiter::direct(
            Quota::per_second(NonZeroU32::new(rps).with_context(|| anyhow!("rps is zero"))?)
                .allow_burst(NonZeroU32::new(burst).with_context(|| anyhow!("burst is zero"))?),
        )
        .with_middleware::<StateInformationMiddleware>();
        let policy = HeaderValue::from_str(&format!("\"burst\";q={burst};w={rps}"))
            .context("Failed to create rate limit policy")?;
        Ok(Self {
            bucket,
            policy,
            burst: burst.into(),
        })
    }
}

impl RateLimiterMap {
    /// Build from `(key, `[`RateLimitConfig`]`)` pairs, rendering rejections under
    /// `scope`.
    ///
    /// # Errors
    /// Returns an error if any `rps` or `burst` is 0.
    pub fn from_pairs(
        scope: &'static str,
        pairs: impl IntoIterator<Item = (RateLimitKey, RateLimitConfig)>,
    ) -> Result<Self> {
        let mut buckets = HashMap::new();
        let mut inflight = HashMap::new();
        for (key, cfg) in pairs {
            buckets.insert(
                key.clone(),
                Arc::new(
                    BucketMapEntry::new(cfg.rps, cfg.burst)
                        .with_context(|| anyhow!("RateLimit entry invalid for {key:?}"))?,
                ),
            );
            inflight.insert(key, Arc::new(Semaphore::new(cfg.in_flight as usize)));
        }
        Ok(Self {
            buckets: Arc::new(buckets),
            inflight: Arc::new(inflight),
            scope,
        })
    }
}

// TODO: Use tower-governor instead of own implementation (upd: https://github.com/benwis/tower-governor/issues/59 )
/// Rate-limit + in-flight middleware. Emits a canonical `resource_exhausted`
/// Problem under `scope` when the token bucket is exhausted, and a scope-free
/// `service_unavailable` when the in-flight limit is reached.
pub async fn rate_limit_middleware(
    State(map): State<RateLimiterMap>,
    mut req: Request,
    next: Next,
) -> Response {
    let method = req.method().clone();
    // Use MatchedPath extension (set by Axum router) for accurate route matching
    let path = req
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map_or_else(|| req.uri().path().to_owned(), |p| p.as_str().to_owned());

    let path = common::resolve_path(&req, path.as_str());

    let key = (method, path);

    if let Some(bucker_map_entry) = map.buckets.get(&key) {
        let headers = req.headers_mut();
        headers.insert("RateLimit-Policy", bucker_map_entry.policy.clone());
        match bucker_map_entry.bucket.check() {
            Ok(state) => {
                headers.insert("RateLimit-Limit", bucker_map_entry.burst.clone());
                headers.insert(
                    "RateLimit-Limit-Remaining",
                    state.remaining_burst_capacity().into(),
                );
                headers.insert("X-RateLimit-Limit", bucker_map_entry.burst.clone());
                headers.insert(
                    "X-RateLimit-Remaining",
                    state.remaining_burst_capacity().into(),
                );
            }
            Err(not_until) => {
                let wait = not_until.wait_time_from(bucker_map_entry.bucket.clock().now());
                let wait_secs = wait.as_secs();
                log_rate_limit_exceeded(&key, wait_secs);
                let policy = bucker_map_entry.policy.clone();
                let burst = bucker_map_entry.burst.clone();
                let err = CanonicalError::scoped_resource_exhausted(map.scope)
                    .with_quota_violation("rate_limit", format!("retry_after_seconds={wait_secs}"))
                    .create();
                let mut response = err.into_response();
                let response_headers = response.headers_mut();
                response_headers.insert("RateLimit-Policy", policy);
                response_headers.insert("RateLimit-Limit", burst.clone());
                response_headers.insert("X-RateLimit-Limit", burst);
                if let Ok(retry_after) = HeaderValue::from_str(&wait_secs.to_string()) {
                    response_headers.insert(header::RETRY_AFTER, retry_after);
                }
                return response;
            }
        }
    }

    if let Some(sem) = map.inflight.get(&key) {
        if let Ok(_permit) = sem.clone().try_acquire_owned() {
            // Allow request; permit is dropped when response future completes
            return next.run(req).await;
        }
        log_in_flight_limit_reached(&key);
        let err = CanonicalError::service_unavailable()
            .with_retry_after_seconds(5)
            .create();
        return err.into_response();
    }

    next.run(req).await
}

fn log_rate_limit_exceeded(key: &RateLimitKey, retry_after_seconds: u64) {
    tracing::debug!(
        method = %key.0,
        path = %key.1,
        retry_after_seconds,
        "rate limit exceeded"
    );
}

fn log_in_flight_limit_reached(key: &RateLimitKey) {
    tracing::debug!(
        method = %key.0,
        path = %key.1,
        "in-flight limit reached: request rejected"
    );
}
