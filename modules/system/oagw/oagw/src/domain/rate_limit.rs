use std::collections::HashSet;
use std::time::{Instant, SystemTime};

use crate::domain::error::DomainError;
use crate::domain::model::{RateLimitAlgorithm, RateLimitConfig, RateLimitScope, Window};
use dashmap::DashMap;
use modkit_macros::domain_model;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Clock abstraction — allows deterministic time control in tests.
// ---------------------------------------------------------------------------

#[cfg(not(test))]
fn now() -> Instant {
    Instant::now()
}

#[cfg(test)]
thread_local! {
    static MOCK_NOW: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn now() -> Instant {
    MOCK_NOW.with(|cell| cell.get().unwrap_or_else(Instant::now))
}

/// Quota metadata returned on successful token consumption.
#[domain_model]
#[derive(Debug, Clone, Copy)]
pub struct RateLimitOutcome {
    /// The bucket capacity (maps to `X-RateLimit-Limit`).
    pub limit: u64,
    /// Remaining tokens after consumption (maps to `X-RateLimit-Remaining`).
    pub remaining: u64,
    /// Unix epoch timestamp when the bucket will be full again (maps to `X-RateLimit-Reset`).
    pub reset_epoch: u64,
}

#[domain_model]
pub struct RateLimiter {
    buckets: DashMap<String, Bucket>,
}

#[domain_model]
struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_rate: f64, // tokens per second
    last_refill: Instant,
}

impl TokenBucket {
    fn new(config: &RateLimitConfig) -> Self {
        let capacity = config
            .burst
            .as_ref()
            .map_or(config.sustained.rate as f64, |b| b.capacity as f64);
        let window_secs = window_to_secs(&config.sustained.window);
        let refill_rate = config.sustained.rate as f64 / window_secs;
        Self {
            capacity,
            tokens: capacity,
            refill_rate,
            last_refill: now(),
        }
    }

    fn refill(&mut self) {
        let t = now();
        let elapsed = t.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.capacity);
        self.last_refill = t;
    }

    fn try_consume(&mut self, cost: f64) -> bool {
        self.refill();
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }

    fn retry_after_secs(&self, cost: f64) -> u64 {
        if self.refill_rate <= 0.0 {
            return 60;
        }
        let needed = cost - self.tokens;
        if needed <= 0.0 {
            return 0;
        }
        (needed / self.refill_rate).ceil() as u64
    }
}

// ---------------------------------------------------------------------------
// Sliding Window — sub-window counting algorithm
// ---------------------------------------------------------------------------

/// Number of sub-windows per window size. More sub-windows = higher precision
/// at the cost of slightly more memory per bucket.
///
/// | Window | Sub-windows | Sub-window duration | Precision |
/// |--------|-------------|---------------------|-----------|
/// | Second | 10          | 100ms               | 100ms     |
/// | Minute | 60          | 1s                  | 1s        |
/// | Hour   | 60          | 1min                | 1min      |
/// | Day    | 144         | 10min               | 10min     |
fn sub_windows_for(window: &Window) -> usize {
    match window {
        Window::Second => 10,
        Window::Minute => 60,
        Window::Hour => 60,
        Window::Day => 144,
    }
}

fn window_to_nanos(window: &Window) -> u64 {
    match window {
        Window::Second => 1_000_000_000,
        Window::Minute => 60_000_000_000,
        Window::Hour => 3_600_000_000_000,
        Window::Day => 86_400_000_000_000,
    }
}

#[domain_model]
struct SlidingWindowBucket {
    /// Maximum requests allowed per full window (`sustained.rate`).
    /// Burst config is intentionally ignored — sliding window enforces strict limits.
    limit: u32,
    /// Ring buffer of sub-window request counters.
    counters: Vec<u32>,
    /// Number of sub-windows in the ring buffer.
    num_sub_windows: usize,
    /// Duration of each sub-window in nanoseconds.
    sub_window_nanos: u64,
    /// Index of the current (newest) sub-window in the ring buffer.
    current_index: usize,
    /// When the current sub-window started.
    current_start: Instant,
    /// Cached sum of all counters (avoids O(n) sum on every check).
    total_count: u32,
}

impl SlidingWindowBucket {
    fn new(config: &RateLimitConfig) -> Self {
        let n = sub_windows_for(&config.sustained.window);
        let total_nanos = window_to_nanos(&config.sustained.window);
        Self {
            limit: config.sustained.rate,
            counters: vec![0; n],
            num_sub_windows: n,
            sub_window_nanos: total_nanos / n as u64,
            current_index: 0,
            current_start: now(),
            total_count: 0,
        }
    }

    /// Advance the ring buffer to the current time, zeroing expired sub-windows.
    fn advance(&mut self) {
        let elapsed_nanos = now().duration_since(self.current_start).as_nanos() as u64;
        let steps = (elapsed_nanos / self.sub_window_nanos) as usize;
        if steps == 0 {
            return;
        }
        if steps >= self.num_sub_windows {
            for c in &mut self.counters {
                *c = 0;
            }
            self.total_count = 0;
            self.current_index = 0;
            self.current_start = now();
            return;
        }
        for _ in 0..steps {
            self.current_index = (self.current_index + 1) % self.num_sub_windows;
            self.total_count -= self.counters[self.current_index];
            self.counters[self.current_index] = 0;
        }
        self.current_start += std::time::Duration::from_nanos(self.sub_window_nanos * steps as u64);
    }

    fn try_consume(&mut self, cost: u32) -> bool {
        self.advance();
        if self.total_count + cost <= self.limit {
            self.counters[self.current_index] += cost;
            self.total_count += cost;
            true
        } else {
            false
        }
    }

    fn remaining(&self) -> u32 {
        self.limit.saturating_sub(self.total_count)
    }

    /// Seconds until enough sub-windows expire to free `cost` capacity.
    fn retry_after_secs(&self, cost: u32) -> u64 {
        let needed = (self.total_count + cost).saturating_sub(self.limit);
        if needed == 0 {
            return 0;
        }
        let elapsed_nanos = now().duration_since(self.current_start).as_nanos() as u64;
        let remaining_nanos = self.sub_window_nanos.saturating_sub(elapsed_nanos);
        let mut freed = 0u32;
        // Walk from oldest sub-window; each step expires one sub-window.
        for i in 0..self.num_sub_windows {
            let idx = (self.current_index + 1 + i) % self.num_sub_windows;
            freed += self.counters[idx];
            if freed >= needed {
                let wait_nanos = remaining_nanos + (i as u64) * self.sub_window_nanos;
                return (wait_nanos as f64 / 1_000_000_000.0).ceil().max(1.0) as u64;
            }
        }
        // Fallback: full window.
        (self.num_sub_windows as u64 * self.sub_window_nanos / 1_000_000_000).max(1)
    }
}

// ---------------------------------------------------------------------------
// Bucket — enum wrapper for algorithm dispatch
// ---------------------------------------------------------------------------

enum Bucket {
    Token(TokenBucket),
    Sliding(SlidingWindowBucket),
}

/// The resource type that owns the rate-limit configuration.
pub enum RateLimitResource {
    Upstream,
    Route,
}

/// Context needed to build a scope-aware rate-limit key.
pub struct RateLimitKeyContext<'a> {
    pub resource: RateLimitResource,
    pub resource_id: &'a Uuid,
    pub scope: &'a RateLimitScope,
    pub tenant_id: &'a Uuid,
    pub subject_id: &'a Uuid,
    pub client_ip: Option<&'a str>,
    pub window: &'a Window,
}

/// Build a rate-limit bucket key based on the configured scope.
///
/// Key format: `oagw:ratelimit:{resource_type}:{resource_id}:{scope}:{scope_id}:{window}`
///
/// The resource type and id are placed first so that all keys for a given
/// resource share a common prefix. This enables efficient prefix-based cleanup
/// in both the in-memory `DashMap` and Redis (`SCAN`).
///
/// For in-memory token buckets the window segment contains only the time unit
/// (e.g. `second`). Phase 8 (Redis) will append the time-bucket ID
/// (e.g. `minute:202601301530`).
///
/// **IP scope fallback:** When `scope` is `Ip` and `client_ip` is `None`
/// (i.e. neither `X-Forwarded-For` nor `X-Real-IP` headers are present), the
/// key uses the literal `"unknown"` as the scope identifier. This causes all
/// such requests to share a single bucket, effectively degrading to global
/// scope for the resource.
pub fn build_rate_limit_key(ctx: &RateLimitKeyContext<'_>) -> String {
    let res = match ctx.resource {
        RateLimitResource::Upstream => "upstream",
        RateLimitResource::Route => "route",
    };
    let w = window_label(ctx.window);
    match ctx.scope {
        RateLimitScope::Global => {
            format!("oagw:ratelimit:{res}:{}:global:{w}", ctx.resource_id)
        }
        RateLimitScope::Tenant => {
            format!(
                "oagw:ratelimit:{res}:{}:tenant:{}:{w}",
                ctx.resource_id, ctx.tenant_id
            )
        }
        RateLimitScope::User => {
            format!(
                "oagw:ratelimit:{res}:{}:user:{}:{w}",
                ctx.resource_id, ctx.subject_id
            )
        }
        // When client_ip is None (no X-Forwarded-For / X-Real-IP), falls back to
        // "unknown" — all such requests share one bucket (effectively global scope).
        RateLimitScope::Ip => {
            format!(
                "oagw:ratelimit:{res}:{}:ip:{}:{w}",
                ctx.resource_id,
                ctx.client_ip.unwrap_or("unknown")
            )
        }
        RateLimitScope::Route => {
            format!("oagw:ratelimit:{res}:{}:route:{w}", ctx.resource_id)
        }
    }
}

fn window_label(window: &Window) -> &'static str {
    match window {
        Window::Second => "second",
        Window::Minute => "minute",
        Window::Hour => "hour",
        Window::Day => "day",
    }
}

fn window_to_secs(window: &Window) -> f64 {
    match window {
        Window::Second => 1.0,
        Window::Minute => 60.0,
        Window::Hour => 3600.0,
        Window::Day => 86400.0,
    }
}

impl RateLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buckets: DashMap::new(),
        }
    }

    /// Remove all entries whose keys are not in `active_keys`.
    #[allow(dead_code)]
    pub fn purge_keys(&self, active_keys: &HashSet<String>) {
        self.buckets.retain(|k, _| active_keys.contains(k));
    }

    /// Remove all rate-limit buckets associated with an upstream (across all scopes).
    pub fn remove_keys_for_upstream(&self, upstream_id: Uuid) {
        let prefix = format!("oagw:ratelimit:upstream:{upstream_id}:");
        self.buckets.retain(|k, _| !k.starts_with(&prefix));
    }

    /// Remove all rate-limit buckets associated with a route.
    pub fn remove_keys_for_route(&self, route_id: Uuid) {
        let prefix = format!("oagw:ratelimit:route:{route_id}:");
        self.buckets.retain(|k, _| !k.starts_with(&prefix));
    }

    /// Try to consume tokens for the given key.
    ///
    /// Dispatches to the appropriate algorithm (`TokenBucket` or `SlidingWindow`)
    /// based on `config.algorithm`.
    ///
    /// # Errors
    /// Returns `DomainError::RateLimitExceeded` with Retry-After seconds when exhausted.
    pub fn try_consume(
        &self,
        key: &str,
        config: &RateLimitConfig,
        instance_uri: &str,
    ) -> Result<RateLimitOutcome, DomainError> {
        let mut entry =
            self.buckets
                .entry(key.to_string())
                .or_insert_with(|| match config.algorithm {
                    RateLimitAlgorithm::TokenBucket => Bucket::Token(TokenBucket::new(config)),
                    RateLimitAlgorithm::SlidingWindow => {
                        Bucket::Sliding(SlidingWindowBucket::new(config))
                    }
                });

        let now_epoch = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        match &mut *entry {
            Bucket::Token(bucket) => {
                let cost = config.cost as f64;
                if bucket.try_consume(cost) {
                    let limit = bucket.capacity as u64;
                    let remaining = bucket.tokens.floor().max(0.0) as u64;
                    let secs_to_full = if bucket.refill_rate > 0.0 {
                        ((bucket.capacity - bucket.tokens) / bucket.refill_rate).ceil() as u64
                    } else {
                        0
                    };
                    Ok(RateLimitOutcome {
                        limit,
                        remaining,
                        reset_epoch: now_epoch + secs_to_full,
                    })
                } else {
                    let retry_after = bucket.retry_after_secs(cost);
                    let limit = bucket.capacity as u64;
                    let remaining = bucket.tokens.floor().max(0.0) as u64;
                    let secs_to_full = if bucket.refill_rate > 0.0 {
                        ((bucket.capacity - bucket.tokens) / bucket.refill_rate).ceil() as u64
                    } else {
                        0
                    };
                    Err(DomainError::RateLimitExceeded {
                        detail: format!("rate limit exceeded for key: {key}"),
                        instance: instance_uri.to_string(),
                        retry_after_secs: Some(retry_after),
                        limit: if config.response_headers {
                            Some(limit)
                        } else {
                            None
                        },
                        remaining: if config.response_headers {
                            Some(remaining)
                        } else {
                            None
                        },
                        reset_epoch: if config.response_headers {
                            Some(now_epoch + secs_to_full)
                        } else {
                            None
                        },
                    })
                }
            }
            Bucket::Sliding(bucket) => {
                let cost = config.cost;
                if bucket.try_consume(cost) {
                    let limit = bucket.limit as u64;
                    let remaining = bucket.remaining() as u64;
                    let window_secs = window_to_secs(&config.sustained.window).ceil() as u64;
                    Ok(RateLimitOutcome {
                        limit,
                        remaining,
                        reset_epoch: now_epoch
                            + if bucket.total_count > 0 {
                                window_secs
                            } else {
                                0
                            },
                    })
                } else {
                    let retry_after = bucket.retry_after_secs(cost);
                    let limit = bucket.limit as u64;
                    let remaining = bucket.remaining() as u64;
                    let window_secs = window_to_secs(&config.sustained.window).ceil() as u64;
                    Err(DomainError::RateLimitExceeded {
                        detail: format!("rate limit exceeded for key: {key}"),
                        instance: instance_uri.to_string(),
                        retry_after_secs: Some(retry_after),
                        limit: if config.response_headers {
                            Some(limit)
                        } else {
                            None
                        },
                        remaining: if config.response_headers {
                            Some(remaining)
                        } else {
                            None
                        },
                        reset_epoch: if config.response_headers {
                            Some(now_epoch + window_secs)
                        } else {
                            None
                        },
                    })
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::domain::model::{
        BurstConfig, RateLimitAlgorithm, RateLimitScope, RateLimitStrategy, SustainedRate,
    };
    use uuid::Uuid;

    use super::*;

    fn make_config(rate: u32, window: Window, burst_capacity: Option<u32>) -> RateLimitConfig {
        RateLimitConfig {
            sharing: Default::default(),
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate { rate, window },
            burst: burst_capacity.map(|c| BurstConfig { capacity: c }),
            budget: None,
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: true,
            pool_owner_id: None,
        }
    }

    #[test]
    fn allows_within_capacity() {
        let limiter = RateLimiter::new();
        let config = make_config(10, Window::Second, None);
        for _ in 0..10 {
            assert!(limiter.try_consume("test", &config, "/test").is_ok());
        }
    }

    #[test]
    fn denies_when_exhausted() {
        let limiter = RateLimiter::new();
        let config = make_config(2, Window::Second, None);
        assert!(limiter.try_consume("test", &config, "/test").is_ok());
        assert!(limiter.try_consume("test", &config, "/test").is_ok());
        let err = limiter.try_consume("test", &config, "/test").unwrap_err();
        assert!(matches!(err, DomainError::RateLimitExceeded { .. }));
    }

    #[test]
    fn retry_after_is_calculated() {
        let limiter = RateLimiter::new();
        let config = make_config(1, Window::Minute, None);
        assert!(limiter.try_consume("test", &config, "/test").is_ok());
        match limiter.try_consume("test", &config, "/test") {
            Err(DomainError::RateLimitExceeded {
                retry_after_secs, ..
            }) => {
                // ~60 seconds (1 token per minute).
                assert!(retry_after_secs.unwrap() > 0);
                assert!(retry_after_secs.unwrap() <= 60);
            }
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn burst_capacity_used() {
        let limiter = RateLimiter::new();
        let config = make_config(1, Window::Second, Some(5));
        for _ in 0..5 {
            assert!(limiter.try_consume("test", &config, "/test").is_ok());
        }
        assert!(limiter.try_consume("test", &config, "/test").is_err());
    }

    #[test]
    fn separate_keys_independent() {
        let limiter = RateLimiter::new();
        let config = make_config(1, Window::Second, None);
        assert!(limiter.try_consume("key-a", &config, "/test").is_ok());
        assert!(limiter.try_consume("key-b", &config, "/test").is_ok());
        assert!(limiter.try_consume("key-a", &config, "/test").is_err());
        assert!(limiter.try_consume("key-b", &config, "/test").is_err());
    }

    #[test]
    fn purge_removes_stale_entries() {
        let limiter = RateLimiter::new();
        let config = make_config(10, Window::Second, None);
        limiter.try_consume("a", &config, "/test").unwrap();
        limiter.try_consume("b", &config, "/test").unwrap();
        limiter.try_consume("c", &config, "/test").unwrap();

        let active: HashSet<String> = ["a", "c"].iter().map(|s| (*s).into()).collect();
        limiter.purge_keys(&active);

        // a and c survive, b is gone.
        assert!(limiter.buckets.contains_key("a"));
        assert!(!limiter.buckets.contains_key("b"));
        assert!(limiter.buckets.contains_key("c"));
    }

    #[test]
    fn purge_with_empty_set_removes_all() {
        let limiter = RateLimiter::new();
        let config = make_config(10, Window::Second, None);
        limiter.try_consume("x", &config, "/test").unwrap();
        limiter.try_consume("y", &config, "/test").unwrap();

        limiter.purge_keys(&HashSet::new());

        assert!(limiter.buckets.is_empty());
    }

    #[test]
    fn try_consume_returns_outcome_metadata() {
        let limiter = RateLimiter::new();
        let config = make_config(10, Window::Second, Some(10));

        let outcome = limiter.try_consume("test", &config, "/test").unwrap();
        assert_eq!(outcome.limit, 10);
        assert_eq!(outcome.remaining, 9);

        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(outcome.reset_epoch >= now_epoch);
        assert!(outcome.reset_epoch <= now_epoch + 2);

        // Consume more and verify remaining decreases.
        let outcome2 = limiter.try_consume("test", &config, "/test").unwrap();
        assert_eq!(outcome2.remaining, 8);
    }

    #[test]
    fn error_includes_rate_limit_metadata() {
        let limiter = RateLimiter::new();
        let config = make_config(1, Window::Second, Some(1));
        limiter.try_consume("test", &config, "/test").unwrap();

        match limiter.try_consume("test", &config, "/test") {
            Err(DomainError::RateLimitExceeded {
                limit,
                remaining,
                reset_epoch,
                ..
            }) => {
                assert_eq!(limit, Some(1));
                assert_eq!(remaining, Some(0));
                let now_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                assert!(reset_epoch.unwrap() >= now_epoch);
            }
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn outcome_limit_falls_back_to_sustained_rate_without_burst() {
        let limiter = RateLimiter::new();
        let config = make_config(5, Window::Second, None); // no burst
        let outcome = limiter.try_consume("test", &config, "/test").unwrap();
        assert_eq!(
            outcome.limit, 5,
            "limit should equal sustained rate when burst is absent"
        );
    }

    #[test]
    fn outcome_reset_epoch_in_future() {
        let limiter = RateLimiter::new();
        let config = make_config(10, Window::Second, Some(10));
        // Consume 5 tokens so the bucket is partially drained.
        for _ in 0..5 {
            limiter.try_consume("test", &config, "/test").unwrap();
        }
        let outcome = limiter.try_consume("test", &config, "/test").unwrap();
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(
            outcome.reset_epoch >= now_epoch,
            "reset_epoch should be >= now"
        );
    }

    #[test]
    fn error_reset_epoch_is_time_until_full() {
        let limiter = RateLimiter::new();
        // capacity=5, rate=5/min → refill_rate ≈ 0.083 tok/s.
        // Consuming all 5 means secs_to_full ≈ 60s, but retry_after ≈ 12s (1 token).
        let config = make_config(5, Window::Minute, Some(5));
        for _ in 0..5 {
            limiter.try_consume("test", &config, "/test").unwrap();
        }

        match limiter.try_consume("test", &config, "/test") {
            Err(DomainError::RateLimitExceeded {
                retry_after_secs,
                reset_epoch,
                ..
            }) => {
                let now_epoch = std::time::SystemTime::now()
                    .duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let retry = retry_after_secs.unwrap();
                let reset = reset_epoch.unwrap();

                // reset_epoch should reflect time-until-full (≈60s), not retry_after (≈12s).
                assert!(
                    reset >= now_epoch + 50,
                    "reset_epoch ({reset}) should be ≈60s from now ({now_epoch}), \
                     not ≈retry_after ({retry}s)"
                );
                assert!(
                    retry < 20,
                    "retry_after ({retry}s) should be much less than secs_to_full"
                );
            }
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn cost_greater_than_one_consumes_multiple_tokens() {
        let limiter = RateLimiter::new();
        let mut config = make_config(100, Window::Second, Some(10));
        config.cost = 3;

        // 10 tokens, cost=3: should allow 3 requests (9 tokens), fail on 4th (1 < 3).
        let o1 = limiter.try_consume("test", &config, "/test").unwrap();
        assert_eq!(o1.remaining, 7);

        let o2 = limiter.try_consume("test", &config, "/test").unwrap();
        assert_eq!(o2.remaining, 4);

        let o3 = limiter.try_consume("test", &config, "/test").unwrap();
        assert_eq!(o3.remaining, 1);

        let err = limiter.try_consume("test", &config, "/test").unwrap_err();
        assert!(matches!(err, DomainError::RateLimitExceeded { .. }));
    }

    #[test]
    fn error_omits_metadata_when_response_headers_false() {
        let limiter = RateLimiter::new();
        let mut config = make_config(1, Window::Minute, Some(1));
        config.response_headers = false;

        // Exhaust the bucket.
        limiter.try_consume("test", &config, "/test").unwrap();

        // Second request should be rejected.
        let err = limiter.try_consume("test", &config, "/test").unwrap_err();
        match err {
            DomainError::RateLimitExceeded {
                retry_after_secs,
                limit,
                remaining,
                reset_epoch,
                ..
            } => {
                assert!(
                    retry_after_secs.is_some(),
                    "retry_after_secs must always be present regardless of response_headers"
                );
                assert!(
                    limit.is_none(),
                    "limit must be None when response_headers is false"
                );
                assert!(
                    remaining.is_none(),
                    "remaining must be None when response_headers is false"
                );
                assert!(
                    reset_epoch.is_none(),
                    "reset_epoch must be None when response_headers is false"
                );
            }
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }
    }

    // --- build_rate_limit_key tests ---

    fn upstream_ctx<'a>(
        id: &'a Uuid,
        scope: &'a RateLimitScope,
        tenant_id: &'a Uuid,
        subject_id: &'a Uuid,
        client_ip: Option<&'a str>,
        window: &'a Window,
    ) -> RateLimitKeyContext<'a> {
        RateLimitKeyContext {
            resource: RateLimitResource::Upstream,
            resource_id: id,
            scope,
            tenant_id,
            subject_id,
            client_ip,
            window,
        }
    }

    fn route_ctx<'a>(
        id: &'a Uuid,
        scope: &'a RateLimitScope,
        tenant_id: &'a Uuid,
        subject_id: &'a Uuid,
        client_ip: Option<&'a str>,
        window: &'a Window,
    ) -> RateLimitKeyContext<'a> {
        RateLimitKeyContext {
            resource: RateLimitResource::Route,
            resource_id: id,
            scope,
            tenant_id,
            subject_id,
            client_ip,
            window,
        }
    }

    #[test]
    fn build_key_global() {
        let uid = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let zero = Uuid::nil();
        let key = build_rate_limit_key(&upstream_ctx(
            &uid,
            &RateLimitScope::Global,
            &zero,
            &zero,
            None,
            &Window::Second,
        ));
        assert_eq!(key, format!("oagw:ratelimit:upstream:{uid}:global:second"));
    }

    #[test]
    fn build_key_tenant() {
        let uid = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let tid = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let zero = Uuid::nil();
        let key = build_rate_limit_key(&upstream_ctx(
            &uid,
            &RateLimitScope::Tenant,
            &tid,
            &zero,
            None,
            &Window::Minute,
        ));
        assert_eq!(
            key,
            format!("oagw:ratelimit:upstream:{uid}:tenant:{tid}:minute")
        );
    }

    #[test]
    fn build_key_user() {
        let uid = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let sid = Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap();
        let zero = Uuid::nil();
        let key = build_rate_limit_key(&upstream_ctx(
            &uid,
            &RateLimitScope::User,
            &zero,
            &sid,
            None,
            &Window::Hour,
        ));
        assert_eq!(
            key,
            format!("oagw:ratelimit:upstream:{uid}:user:{sid}:hour")
        );
    }

    #[test]
    fn build_key_ip_present() {
        let uid = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let zero = Uuid::nil();
        let key = build_rate_limit_key(&upstream_ctx(
            &uid,
            &RateLimitScope::Ip,
            &zero,
            &zero,
            Some("192.168.1.1"),
            &Window::Day,
        ));
        assert_eq!(
            key,
            format!("oagw:ratelimit:upstream:{uid}:ip:192.168.1.1:day")
        );
    }

    #[test]
    fn build_key_ip_absent() {
        let uid = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();
        let zero = Uuid::nil();
        let key = build_rate_limit_key(&upstream_ctx(
            &uid,
            &RateLimitScope::Ip,
            &zero,
            &zero,
            None,
            &Window::Second,
        ));
        assert_eq!(
            key,
            format!("oagw:ratelimit:upstream:{uid}:ip:unknown:second")
        );
    }

    #[test]
    fn build_key_route_resource() {
        let rid = Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();
        let tid = Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap();
        let zero = Uuid::nil();
        let key = build_rate_limit_key(&route_ctx(
            &rid,
            &RateLimitScope::Tenant,
            &tid,
            &zero,
            None,
            &Window::Second,
        ));
        assert_eq!(
            key,
            format!("oagw:ratelimit:route:{rid}:tenant:{tid}:second")
        );
    }

    #[test]
    fn build_key_route_scope() {
        let rid = Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();
        let zero = Uuid::nil();
        let key = build_rate_limit_key(&route_ctx(
            &rid,
            &RateLimitScope::Route,
            &zero,
            &zero,
            None,
            &Window::Second,
        ));
        assert_eq!(key, format!("oagw:ratelimit:route:{rid}:route:second"));
    }

    #[test]
    fn remove_keys_for_upstream_cleans_all_scopes() {
        let limiter = RateLimiter::new();
        let config = make_config(10, Window::Second, None);
        let uid = Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap();

        // All scopes share the prefix `oagw:ratelimit:upstream:{uid}:`.
        let keys = [
            format!("oagw:ratelimit:upstream:{uid}:global:second"),
            format!("oagw:ratelimit:upstream:{uid}:tenant:aaa:minute"),
            format!("oagw:ratelimit:upstream:{uid}:user:bbb:hour"),
            format!("oagw:ratelimit:upstream:{uid}:ip:1.2.3.4:day"),
        ];
        for k in &keys {
            limiter.try_consume(k, &config, "/test").unwrap();
        }
        // Unrelated upstream key that should survive.
        let other = "oagw:ratelimit:upstream:00000000-0000-0000-0000-000000000099:global:second";
        limiter.try_consume(other, &config, "/test").unwrap();

        limiter.remove_keys_for_upstream(uid);

        for k in &keys {
            assert!(
                !limiter.buckets.contains_key(k.as_str()),
                "key {k} should be removed"
            );
        }
        assert!(limiter.buckets.contains_key(other));
    }

    #[test]
    fn remove_keys_for_route_cleans_only_route() {
        let limiter = RateLimiter::new();
        let config = make_config(10, Window::Second, None);
        let rid = Uuid::parse_str("00000000-0000-0000-0000-000000000004").unwrap();

        let route_key = format!("oagw:ratelimit:route:{rid}:tenant:aaa:second");
        let upstream_key =
            "oagw:ratelimit:upstream:00000000-0000-0000-0000-000000000001:global:second";
        limiter.try_consume(&route_key, &config, "/test").unwrap();
        limiter.try_consume(upstream_key, &config, "/test").unwrap();

        limiter.remove_keys_for_route(rid);

        assert!(!limiter.buckets.contains_key(route_key.as_str()));
        assert!(limiter.buckets.contains_key(upstream_key));
    }

    // --- Sliding Window tests ---

    fn make_sliding_config(rate: u32, window: Window) -> RateLimitConfig {
        RateLimitConfig {
            sharing: Default::default(),
            algorithm: RateLimitAlgorithm::SlidingWindow,
            sustained: SustainedRate { rate, window },
            burst: None,
            budget: None,
            scope: RateLimitScope::Tenant,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: true,
            pool_owner_id: None,
        }
    }

    fn set_mock_time(t: Instant) {
        MOCK_NOW.with(|cell| cell.set(Some(t)));
    }

    fn clear_mock_time() {
        MOCK_NOW.with(|cell| cell.set(None));
    }

    #[test]
    fn sliding_window_allows_within_rate() {
        let limiter = RateLimiter::new();
        let config = make_sliding_config(10, Window::Second);
        for _ in 0..10 {
            assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        }
    }

    #[test]
    fn sliding_window_denies_when_exhausted() {
        let limiter = RateLimiter::new();
        let config = make_sliding_config(2, Window::Second);
        assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        let err = limiter.try_consume("sw", &config, "/test").unwrap_err();
        assert!(matches!(err, DomainError::RateLimitExceeded { .. }));
    }

    #[test]
    fn sliding_window_boundary_burst_prevented() {
        let t0 = Instant::now();
        set_mock_time(t0);

        let limiter = RateLimiter::new();
        let config = make_sliding_config(10, Window::Second);

        // Fill the window at t0 (all 10 land in sub-window 0).
        for _ in 0..10 {
            assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        }

        // Advance just past one sub-window boundary (100ms for Second/10 sub-windows).
        // Only sub-window 0 is active with 10 counts; the window hasn't expired.
        let one_sub = std::time::Duration::from_millis(100);
        set_mock_time(t0 + one_sub);

        // New requests should be rejected — the 10 counts from sub-window 0 haven't
        // expired yet (only 1 of 10 sub-windows has rotated, but sub-window 0's
        // counts are still within the sliding window).
        let err = limiter.try_consume("sw", &config, "/test").unwrap_err();
        assert!(matches!(err, DomainError::RateLimitExceeded { .. }));

        clear_mock_time();
    }

    #[test]
    fn sliding_window_ignores_burst_config() {
        let limiter = RateLimiter::new();
        let mut config = make_sliding_config(10, Window::Second);
        config.burst = Some(BurstConfig { capacity: 50 });

        // Limit should be sustained.rate (10), not burst (50).
        for _ in 0..10 {
            assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        }
        let err = limiter.try_consume("sw", &config, "/test").unwrap_err();
        assert!(matches!(err, DomainError::RateLimitExceeded { .. }));
    }

    #[test]
    fn sliding_window_retry_after_calculated() {
        let limiter = RateLimiter::new();
        let config = make_sliding_config(1, Window::Minute);
        assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        match limiter.try_consume("sw", &config, "/test") {
            Err(DomainError::RateLimitExceeded {
                retry_after_secs, ..
            }) => {
                let retry = retry_after_secs.unwrap();
                assert!(retry >= 1, "retry_after should be >= 1s, got {retry}");
                assert!(retry <= 60, "retry_after should be <= 60s, got {retry}");
            }
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }
    }

    #[test]
    fn sliding_window_remaining_decreases() {
        let limiter = RateLimiter::new();
        let config = make_sliding_config(10, Window::Second);

        let o1 = limiter.try_consume("sw", &config, "/test").unwrap();
        assert_eq!(o1.limit, 10);
        assert_eq!(o1.remaining, 9);

        let o2 = limiter.try_consume("sw", &config, "/test").unwrap();
        assert_eq!(o2.remaining, 8);
    }

    #[test]
    fn sliding_window_cost_greater_than_one() {
        let limiter = RateLimiter::new();
        let mut config = make_sliding_config(10, Window::Second);
        config.cost = 3;

        // 10 capacity, cost=3: allows 3 requests (9 used), fails on 4th (1 < 3).
        let o1 = limiter.try_consume("sw", &config, "/test").unwrap();
        assert_eq!(o1.remaining, 7);
        let o2 = limiter.try_consume("sw", &config, "/test").unwrap();
        assert_eq!(o2.remaining, 4);
        let o3 = limiter.try_consume("sw", &config, "/test").unwrap();
        assert_eq!(o3.remaining, 1);
        assert!(limiter.try_consume("sw", &config, "/test").is_err());
    }

    #[test]
    fn sliding_window_separate_keys_independent() {
        let limiter = RateLimiter::new();
        let config = make_sliding_config(1, Window::Second);
        assert!(limiter.try_consume("sw-a", &config, "/test").is_ok());
        assert!(limiter.try_consume("sw-b", &config, "/test").is_ok());
        assert!(limiter.try_consume("sw-a", &config, "/test").is_err());
        assert!(limiter.try_consume("sw-b", &config, "/test").is_err());
    }

    #[test]
    fn sliding_window_recovery_after_full_window() {
        let t0 = Instant::now();
        set_mock_time(t0);

        let limiter = RateLimiter::new();
        let config = make_sliding_config(5, Window::Second);

        // Exhaust the window.
        for _ in 0..5 {
            assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        }
        assert!(limiter.try_consume("sw", &config, "/test").is_err());

        // Advance past the full window — all sub-windows expire.
        set_mock_time(t0 + std::time::Duration::from_secs(2));
        for _ in 0..5 {
            assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        }

        clear_mock_time();
    }

    #[test]
    fn sliding_window_sub_window_granular_recovery() {
        let t0 = Instant::now();
        set_mock_time(t0);

        let limiter = RateLimiter::new();
        // 10 per second, 10 sub-windows → each sub-window is 100ms.
        let config = make_sliding_config(10, Window::Second);

        // Fill sub-window 0 with 5 requests, then advance to sub-window 1 and fill
        // with 5 more. Total = 10, window is full.
        for _ in 0..5 {
            assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        }
        set_mock_time(t0 + std::time::Duration::from_millis(100));
        for _ in 0..5 {
            assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        }
        assert!(limiter.try_consume("sw", &config, "/test").is_err());

        // Advance so sub-window 0 (with 5 counts) rotates out.
        // We need to be at sub-window index 10 relative to start, which means
        // 10 sub-windows = 1000ms from t0. But only 9 more sub-windows need to
        // pass from the current position (sub-window 1). So advance to t0 + 1000ms.
        set_mock_time(t0 + std::time::Duration::from_millis(1000));

        // Sub-window 0's 5 counts have expired, freeing 5 capacity.
        // Sub-window 1's 5 counts are still active. So we have 5 remaining.
        for _ in 0..5 {
            assert!(limiter.try_consume("sw", &config, "/test").is_ok());
        }
        assert!(limiter.try_consume("sw", &config, "/test").is_err());

        clear_mock_time();
    }
}
