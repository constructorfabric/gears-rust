//! The pre-engine request limiter (Foundation contract §3.7, D-185; S2-12).
//!
//! Engine-entering write requests are bounded **before** the engine, because a refusal inside
//! the engine writes the very audit row the limiter exists to prevent. Two limits:
//!
//! * **Per caller — at most 200 per 60 s**, adopted from the platform api-gateway: every
//!   caller-facing engine-entering operation binds the identity-keyed zone [`CALLER_WRITE_ZONE`]
//!   through [`ThrottlingSpec`] (`require_security_context: true`, enforced, never dry-run), and
//!   the five workflow-only operations bind [`WORKFLOW_WRITE_ZONE`] when they are delivered. The
//!   zone values (`3/s`, burst 20; `50/s`, burst 100; 429 with an automatic `Retry-After`) are
//!   gateway configuration, pinned for the E2E host in `config/e2e-orders-lifecycle.yaml`.
//! * **Per (caller, order) — 20 per minute**, the documented gear-local REST-edge fallback of
//!   Q-26 while `cpt-cf-bss-orders-lifecycle-upreq-gateway-path-param-throttle-key` is open: a
//!   keyed limiter at the Orders REST edge, keyed `(subject_id, orderId)`, consulted first in
//!   every existing-order write handler, before boundary validation, authorization, the
//!   registry and the engine. A rejection is the same canonical `resource_exhausted` 429 the
//!   gateway answers, with `Retry-After`, and writes no audit, registry or access-log row.
//!
//! Both are request limiters: every request to a bound operation counts, admitted or refused,
//! so legitimate retries count too. Neither applies to the local SDK, which is in-process
//! traffic rather than gateway traffic, nor to reads, which do not enter the engine.
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use governor::clock::{Clock, DefaultClock, Reference};
use governor::middleware::NoOpMiddleware;
use governor::nanos::Nanos;
use governor::state::StateStore;
use governor::state::keyed::{DashMapStateStore, ShrinkableKeyedStateStore};
use governor::{Quota, RateLimiter};
use toolkit::api::operation_builder::ThrottlingSpec;
use toolkit_canonical_errors::resource_error;
use uuid::Uuid;

/// The identity-keyed gateway zone of the caller-facing engine-entering operations (D-185).
pub const CALLER_WRITE_ZONE: &str = "rl_orders_caller_write";
/// The identity-keyed gateway zone of the five workflow-only operations (D-185): one service
/// principal carries every order's workflow traffic, so it must not share the caller budget.
pub const WORKFLOW_WRITE_ZONE: &str = "rl_orders_workflow_write";

/// The per-(caller, order) baseline of Foundation §3.7: 20 requests per minute.
pub const PER_ORDER_PER_MINUTE_BASELINE: u32 = 20;
/// The default bound on tracked `(caller, order)` keys; beyond it **new** keys are refused until
/// idle keys are reclaimed, so memory stays bounded under attacker-influenced keys while every
/// already-tracked pair keeps its own budget (the api-gateway `max_keys` contract).
pub const PER_ORDER_MAX_KEYS_BASELINE: u64 = 100_000;
/// Idle-key reclamation runs at most this often, and only when the bound is reached: a flood of
/// new keys against a full store must not turn every refusal into a full scan of the store.
const PRUNE_INTERVAL: Duration = Duration::from_secs(1);

/// Canonical scope of the Orders edge limiter's refusals: the order resource.
#[resource_error(toolkit_gts::gts_id!("cf.bss.orders.order.v1~"))]
pub struct OrdersOrderError;

/// The `ThrottlingSpec` every caller-facing engine-entering operation binds: enforced after
/// authentication so the key is the authenticated subject, never dry-run.
#[must_use]
pub fn caller_write_throttling() -> ThrottlingSpec {
    ThrottlingSpec {
        rate_limit_zone: Some(CALLER_WRITE_ZONE.to_owned()),
        in_flight_limit_zone: None,
        require_security_context: true,
        dry_run: false,
    }
}

/// The `ThrottlingSpec` of the workflow-only operations (not yet mounted; S5-08 binds it).
#[must_use]
pub fn workflow_write_throttling() -> ThrottlingSpec {
    ThrottlingSpec {
        rate_limit_zone: Some(WORKFLOW_WRITE_ZONE.to_owned()),
        in_flight_limit_zone: None,
        require_security_context: true,
        dry_run: false,
    }
}

/// Validated per-(caller, order) limiter settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThrottleSettings {
    /// Requests per minute per `(subject_id, orderId)`; the baseline is the maximum.
    pub per_order_per_minute: NonZeroU32,
    /// Bound on tracked keys.
    pub per_order_max_keys: u64,
}
impl Default for ThrottleSettings {
    fn default() -> Self {
        Self {
            per_order_per_minute: NonZeroU32::new(PER_ORDER_PER_MINUTE_BASELINE)
                .unwrap_or(NonZeroU32::MIN),
            per_order_max_keys: PER_ORDER_MAX_KEYS_BASELINE,
        }
    }
}

/// `(subject_id, orderId)`.
type Key = (Uuid, Uuid);

/// The limiter's keyed state, shared between the `governor` limiter and the bound check so the
/// check can tell an already-tracked key (always measured against its own bucket) from a new
/// one (admitted only below the bound). Delegates every store operation to the `DashMap` store
/// the gateway zones use.
#[derive(Clone, Default)]
struct SharedStore(Arc<DashMapStateStore<Key>>);
impl StateStore for SharedStore {
    type Key = Key;
    fn measure_and_replace<T, F, E>(&self, key: &Self::Key, f: F) -> Result<T, E>
    where
        F: Fn(Option<Nanos>) -> Result<(T, Nanos), E>,
    {
        self.0.measure_and_replace(key, f)
    }
}
impl ShrinkableKeyedStateStore<Key> for SharedStore {
    fn retain_recent(&self, drop_below: Nanos) {
        ShrinkableKeyedStateStore::retain_recent(&*self.0, drop_below);
    }
    fn shrink_to_fit(&self) {
        self.0.shrink_to_fit();
    }
    fn len(&self) -> usize {
        self.0.len()
    }
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Why a request was refused at the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// The `(caller, order)` budget is exhausted; retry after the given whole seconds.
    PerOrderLimit { retry_after_seconds: u64 },
    /// The tracked-key bound is reached, the key is new and no idle key could be reclaimed.
    MaxKeys { retry_after_seconds: u64 },
}
impl Rejection {
    #[must_use]
    pub fn retry_after_seconds(self) -> u64 {
        match self {
            Self::PerOrderLimit {
                retry_after_seconds,
            }
            | Self::MaxKeys {
                retry_after_seconds,
            } => retry_after_seconds,
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::PerOrderLimit { .. } => "per-order request limit exceeded",
            Self::MaxKeys { .. } => "per-order limiter key capacity exceeded",
        }
    }
}

/// The gear-local `(subject_id, orderId)` limiter.
///
/// Generic over the `governor` clock so the reclamation and refill behaviour is testable on a
/// fake clock; the runtime uses [`DefaultClock`].
pub struct PerOrderLimiter<C: Clock = DefaultClock> {
    limiter: RateLimiter<Key, SharedStore, C, NoOpMiddleware<C::Instant>>,
    store: SharedStore,
    settings: ThrottleSettings,
    /// When idle keys were last reclaimed (on the limiter's clock); `None` before the first.
    last_prune: Mutex<Option<C::Instant>>,
}
impl<C: Clock> std::fmt::Debug for PerOrderLimiter<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerOrderLimiter")
            .field("settings", &self.settings)
            .field("tracked_keys", &self.store.0.len())
            .finish_non_exhaustive()
    }
}
impl PerOrderLimiter {
    #[must_use]
    pub fn new(settings: ThrottleSettings) -> Arc<Self> {
        Self::with_clock(settings, DefaultClock::default())
    }
}
impl<C: Clock> PerOrderLimiter<C> {
    /// Build the limiter over an explicit clock (the runtime uses [`PerOrderLimiter::new`]).
    #[must_use]
    pub fn with_clock(settings: ThrottleSettings, clock: C) -> Arc<Self> {
        let store = SharedStore::default();
        Arc::new(Self {
            limiter: RateLimiter::new(
                Quota::per_minute(settings.per_order_per_minute),
                store.clone(),
                clock,
            ),
            store,
            settings,
            last_prune: Mutex::new(None),
        })
    }

    #[must_use]
    pub fn settings(&self) -> ThrottleSettings {
        self.settings
    }

    fn is_full(&self) -> bool {
        u64::try_from(self.store.0.len()).unwrap_or(u64::MAX) >= self.settings.per_order_max_keys
    }

    /// Reclaim fully replenished keys, at most once per [`PRUNE_INTERVAL`].
    fn prune_if_due(&self) {
        let now = self.limiter.clock().now();
        let mut last = self
            .last_prune
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let due = last.is_none_or(|at| now.duration_since(at) >= Nanos::from(PRUNE_INTERVAL));
        if due {
            *last = Some(now);
            drop(last);
            self.limiter.retain_recent();
        }
    }

    /// Count one request against `(subject_id, order_id)`; refuse when the pair's budget is
    /// exhausted, or when the pair is new and the key bound is reached with no idle key to
    /// reclaim. An already-tracked pair is never refused by the bound: it is measured against
    /// its own bucket, as under the gateway's `max_keys`. Never touches storage.
    ///
    /// # Errors
    /// The rejection to answer.
    pub fn admit(&self, subject_id: Uuid, order_id: Uuid) -> Result<(), Rejection> {
        let key = (subject_id, order_id);
        if self.is_full() && !self.store.0.contains_key(&key) {
            self.prune_if_due();
            // Re-check after reclamation (or after a concurrent request tracked this key). The
            // bound is approximate under concurrency, like the gateway's: two simultaneous new
            // keys may both take the last slot, never more than that per request.
            if self.is_full() && !self.store.0.contains_key(&key) {
                return Err(Rejection::MaxKeys {
                    retry_after_seconds: 60,
                });
            }
        }
        match self.limiter.check_key(&key) {
            Ok(()) => Ok(()),
            Err(not_until) => {
                let wait = not_until.wait_time_from(self.limiter.clock().now());
                // Round up: a positive sub-second wait must not become 0.
                let retry_after_seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
                Err(Rejection::PerOrderLimit {
                    retry_after_seconds,
                })
            }
        }
    }

    /// Number of tracked keys (tests and metrics).
    #[must_use]
    pub fn tracked_keys(&self) -> usize {
        self.store.0.len()
    }
}

/// The 429 the edge limiter answers: the canonical `resource_exhausted` Problem the gateway
/// uses, scoped to the Orders order resource, with the quota violation subject `throttling`,
/// its retry hint and the `Retry-After` header. The limiter adds no `RateLimit-*` headers of its
/// own: behind the gateway the caller zone decorates this (admitted) response with its own
/// `RateLimit-Remaining`, which describes the caller budget, not the per-order one; the
/// violation description tells the two apart.
#[must_use]
pub fn rejection_response(rejection: Rejection) -> Response {
    let error = OrdersOrderError::resource_exhausted("Orders per-order request limit exceeded")
        .with_quota_violation("throttling", rejection.description())
        .with_quota_violation_retry_after_seconds(rejection.retry_after_seconds())
        .create();
    let mut response = error.into_response();
    *response.status_mut() = StatusCode::TOO_MANY_REQUESTS;
    if let Ok(value) = HeaderValue::from_str(&rejection.retry_after_seconds().to_string()) {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use governor::clock::FakeRelativeClock;

    fn settings(per_minute: u32, max_keys: u64) -> ThrottleSettings {
        ThrottleSettings {
            per_order_per_minute: NonZeroU32::new(per_minute).unwrap(),
            per_order_max_keys: max_keys,
        }
    }

    fn faked(
        per_minute: u32,
        max_keys: u64,
    ) -> (Arc<PerOrderLimiter<FakeRelativeClock>>, FakeRelativeClock) {
        let clock = FakeRelativeClock::default();
        (
            PerOrderLimiter::with_clock(settings(per_minute, max_keys), clock.clone()),
            clock,
        )
    }

    /// The budget is per `(caller, order)`: another order or another caller has its own.
    #[test]
    fn the_budget_is_keyed_by_caller_and_order_and_counts_every_request() {
        let limiter = PerOrderLimiter::new(settings(2, 100));
        let (a, b, o1, o2) = (
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            Uuid::from_u128(10),
            Uuid::from_u128(11),
        );
        assert!(limiter.admit(a, o1).is_ok());
        assert!(limiter.admit(a, o1).is_ok());
        let rejected = limiter.admit(a, o1).unwrap_err();
        assert!(matches!(rejected, Rejection::PerOrderLimit { .. }));
        assert!(rejected.retry_after_seconds() >= 1);
        // A refused request also counted: still refused.
        assert!(limiter.admit(a, o1).is_err());
        assert!(limiter.admit(a, o2).is_ok());
        assert!(limiter.admit(b, o1).is_ok());
        assert_eq!(limiter.tracked_keys(), 3);
    }

    /// The budget refills at the configured per-minute rate; the retry hint is the real wait.
    #[test]
    fn the_budget_refills_on_the_clock_and_the_retry_hint_is_exact() {
        let (limiter, clock) = faked(2, 100);
        let (a, o) = (Uuid::from_u128(1), Uuid::from_u128(10));
        assert!(limiter.admit(a, o).is_ok());
        assert!(limiter.admit(a, o).is_ok());
        // Two per minute: one token every 30 s.
        assert_eq!(
            limiter.admit(a, o).unwrap_err(),
            Rejection::PerOrderLimit {
                retry_after_seconds: 30
            }
        );
        clock.advance(Duration::from_secs(29));
        assert_eq!(
            limiter.admit(a, o).unwrap_err(),
            Rejection::PerOrderLimit {
                retry_after_seconds: 1
            }
        );
        clock.advance(Duration::from_secs(1));
        assert!(limiter.admit(a, o).is_ok());
    }

    /// The key store is bounded: past the bound, **new** keys are refused while every tracked
    /// key keeps its own budget, and reclaimed idle keys reopen admission.
    #[test]
    fn the_key_store_is_bounded_for_new_keys_only() {
        let (limiter, clock) = faked(5, 2);
        let o = Uuid::from_u128(10);
        let (k1, k2, k3) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        assert!(limiter.admit(k1, o).is_ok());
        assert!(limiter.admit(k2, o).is_ok());
        let rejected = limiter.admit(k3, o).unwrap_err();
        assert_eq!(
            rejected,
            Rejection::MaxKeys {
                retry_after_seconds: 60
            }
        );
        assert_eq!(limiter.tracked_keys(), 2);
        // A tracked pair is never refused by the bound: it spends its own budget …
        for _ in 0..4 {
            assert!(limiter.admit(k1, o).is_ok());
        }
        // … and is refused by that budget, not by the bound.
        assert!(matches!(
            limiter.admit(k1, o).unwrap_err(),
            Rejection::PerOrderLimit { .. }
        ));
        // A key is idle once its theoretical arrival time is a full replenish interval in the
        // past (5/min: 12 s per token; `k2` spent one at 0 s, so it is idle from 24 s). Then it
        // is reclaimed and a new key is admitted; the exhausted key is still tracked.
        clock.advance(Duration::from_secs(24));
        assert!(limiter.admit(k3, o).is_ok());
        assert_eq!(limiter.tracked_keys(), 2);
        assert!(matches!(
            limiter.admit(Uuid::from_u128(4), o).unwrap_err(),
            Rejection::MaxKeys { .. }
        ));
    }

    /// Reclamation is rate-limited: a flood of new keys against a full store is refused
    /// without rescanning the store on every request, even when a key has just become idle.
    #[test]
    fn reclamation_runs_at_most_once_per_interval() {
        let (limiter, clock) = faked(5, 1);
        let o = Uuid::from_u128(10);
        let (k1, k2, k3) = (Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3));
        assert!(limiter.admit(k1, o).is_ok());
        // The first refusal prunes (nothing is idle yet).
        assert!(limiter.admit(k2, o).is_err());
        // 24 s: `k1` is idle, the interval has elapsed, so it is reclaimed and `k2` admitted.
        clock.advance(Duration::from_secs(24));
        assert!(limiter.admit(k2, o).is_ok());
        assert_eq!(limiter.tracked_keys(), 1);
        // 47.5 s: a prune runs (due) but `k2` is not idle until 48 s: refused.
        clock.advance(Duration::from_millis(23_500));
        assert!(limiter.admit(k3, o).is_err());
        // 48 s: `k2` is idle now, but the last prune was 0.5 s ago: refused without a rescan,
        // and the store is untouched.
        clock.advance(Duration::from_millis(500));
        assert!(limiter.admit(k3, o).is_err());
        assert_eq!(limiter.tracked_keys(), 1);
        // 48.5 s: the interval has elapsed; the idle key is reclaimed and `k3` admitted.
        clock.advance(Duration::from_millis(500));
        assert!(limiter.admit(k3, o).is_ok());
        assert_eq!(limiter.tracked_keys(), 1);
    }

    /// The refusal is the canonical gateway shape with the Orders order scope.
    #[tokio::test]
    async fn the_rejection_is_the_canonical_resource_exhausted_problem() {
        let response = rejection_response(Rejection::PerOrderLimit {
            retry_after_seconds: 7,
        });
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "7");
        assert_eq!(
            response.headers()["content-type"],
            "application/problem+json"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let problem: toolkit_canonical_errors::Problem = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            problem.problem_type,
            "gts://gts.cf.core.errors.err.v1~cf.core.err.resource_exhausted.v1~"
        );
        let violations = problem.context["violations"].as_array().unwrap();
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0]["subject"], "throttling");
        assert_eq!(violations[0]["retry_after_seconds"], 7);
    }

    /// The gateway bindings are enforced identity-keyed zones, never dry-run or IP-keyed.
    #[test]
    fn the_gateway_bindings_are_enforced_identity_zones() {
        for (spec, zone) in [
            (caller_write_throttling(), CALLER_WRITE_ZONE),
            (workflow_write_throttling(), WORKFLOW_WRITE_ZONE),
        ] {
            assert_eq!(spec.rate_limit_zone.as_deref(), Some(zone));
            assert!(spec.in_flight_limit_zone.is_none());
            assert!(spec.require_security_context);
            assert!(!spec.dry_run);
        }
        assert_ne!(CALLER_WRITE_ZONE, WORKFLOW_WRITE_ZONE);
    }
}
