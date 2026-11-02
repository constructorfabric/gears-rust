//! The per-order and per-caller keyed limiters behind the REST edge and the gate (Foundation
//! contract §3.7, D-185, D-223; S2-12), and the canonical `resource_exhausted` error of a
//! refusal.
//!
//! Lives in `infra` so the gate flows ([`PerCallerLimiter`] for Preview and submit) and the
//! domain need no edge-ward import; the REST adapter's `throttle` module re-exports this module and keeps only
//! the HTTP parts (the gateway zones, `ThrottlingSpec`s and the 429 response). The design notes
//! (what is limited, why before the engine, the metric surface) are on that module.
//!
//! Observability (D-223), after the api-gateway throttling middleware: every refusal counts on
//! `orders_throttle_rejections_total{kind}`, the tracked-key counts are the
//! `orders_throttle_tracked_keys` gauge and the `orders_throttle_caller_tracked_keys{limiter}`
//! gauge, and a full key store logs a warning at most once per reclamation interval. The keys
//! are never a metric attribute and never logged; the `limiter` attribute is the bounded
//! [`CallerLimiter`] token.
use parking_lot::Mutex;
use std::hash::Hash;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use governor::clock::{Clock, DefaultClock, Reference};
use governor::middleware::NoOpMiddleware;
use governor::nanos::Nanos;
use governor::state::StateStore;
use governor::state::keyed::{DashMapStateStore, ShrinkableKeyedStateStore};
use governor::{Quota, RateLimiter};
use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge};
use uuid::Uuid;

use super::error_mapping::OrdersOrderError;

/// The per-(caller, order) baseline of Foundation §3.7: 20 requests per minute.
pub const PER_ORDER_PER_MINUTE_BASELINE: u32 = 20;
/// The default bound on tracked `(caller, order)` keys; beyond it **new** keys are refused until
/// idle keys are reclaimed, so memory stays bounded under attacker-influenced keys while every
/// already-tracked pair keeps its own budget (the api-gateway `max_keys` contract).
pub const PER_ORDER_MAX_KEYS_BASELINE: u64 = 100_000;
/// Idle-key reclamation runs at most this often, and only when the bound is reached: a flood of
/// new keys against a full store must not turn every refusal into a full scan of the store.
const PRUNE_INTERVAL: Duration = Duration::from_secs(1);

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

/// A limiter's keyed state, shared between the `governor` limiter and the bound check so the
/// check can tell an already-tracked key (always measured against its own bucket) from a new
/// one (admitted only below the bound). Delegates every store operation to the `DashMap` store
/// the gateway zones use.
#[derive(Clone)]
struct SharedStore<K: Hash + Eq + Clone>(Arc<DashMapStateStore<K>>);
impl<K: Hash + Eq + Clone> Default for SharedStore<K> {
    fn default() -> Self {
        Self(Arc::new(DashMapStateStore::default()))
    }
}
impl<K: Hash + Eq + Clone> StateStore for SharedStore<K> {
    type Key = K;
    fn measure_and_replace<T, F, E>(&self, key: &Self::Key, f: F) -> Result<T, E>
    where
        F: Fn(Option<Nanos>) -> Result<(T, Nanos), E>,
    {
        self.0.measure_and_replace(key, f)
    }
}
impl<K: Hash + Eq + Clone> ShrinkableKeyedStateStore<K> for SharedStore<K> {
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
    /// A per-caller budget ([`PerCallerLimiter`]) is exhausted; retry after the given seconds.
    PerCallerLimit { retry_after_seconds: u64 },
    /// The per-caller limiter's tracked-key bound is reached for a new caller.
    CallerMaxKeys { retry_after_seconds: u64 },
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
            }
            | Self::PerCallerLimit {
                retry_after_seconds,
            }
            | Self::CallerMaxKeys {
                retry_after_seconds,
            } => retry_after_seconds,
        }
    }
    fn description(self) -> &'static str {
        match self {
            Self::PerOrderLimit { .. } => "per-order request limit exceeded",
            Self::MaxKeys { .. } => "per-order limiter key capacity exceeded",
            Self::PerCallerLimit { .. } => "per-caller request limit exceeded",
            Self::CallerMaxKeys { .. } => "per-caller limiter key capacity exceeded",
        }
    }
    /// The Problem `detail`.
    fn detail(self) -> &'static str {
        match self {
            Self::PerOrderLimit { .. } | Self::MaxKeys { .. } => {
                "Orders per-order request limit exceeded"
            }
            Self::PerCallerLimit { .. } | Self::CallerMaxKeys { .. } => {
                "Orders per-caller request limit exceeded"
            }
        }
    }
    /// The `kind` attribute of the rejection counter.
    fn kind(self) -> &'static str {
        match self {
            Self::PerOrderLimit { .. } => "per_order",
            Self::MaxKeys { .. } => "max_keys",
            Self::PerCallerLimit { .. } => "per_caller",
            Self::CallerMaxKeys { .. } => "caller_max_keys",
        }
    }
}

/// The limiter's instruments on the `bss-orders` process meter (D-223).
struct LimiterSignals {
    rejections: Counter<u64>,
    tracked_keys: Gauge<u64>,
    /// The gauge's attributes: none for the per-order limiter, `limiter` for a per-caller one.
    gauge_attributes: Vec<KeyValue>,
}
impl LimiterSignals {
    fn new() -> Self {
        Self::named(
            "orders_throttle_tracked_keys",
            "(caller, order) keys the Orders edge limiter tracks",
        )
    }
    /// The shared rejection counter and a limiter's own tracked-key gauge.
    fn named(gauge: &'static str, description: &'static str) -> Self {
        let meter = opentelemetry::global::meter("bss-orders");
        Self {
            rejections: meter
                .u64_counter("orders_throttle_rejections_total")
                .with_description(
                    "Orders edge limiter refusals by kind (per_order, max_keys, per_caller, \
                     caller_max_keys)",
                )
                .build(),
            tracked_keys: meter.u64_gauge(gauge).with_description(description).build(),
            gauge_attributes: Vec::new(),
        }
    }
    /// Tag the gauge with the limiter's name (several per-caller limiters share the gauge).
    fn labelled(mut self, limiter: &'static str) -> Self {
        self.gauge_attributes = vec![KeyValue::new("limiter", limiter)];
        self
    }
}

/// The keyed core both limiters share: a `governor` keyed token bucket over a [`SharedStore`],
/// a bound on tracked keys and its paced reclamation, and the D-223 signals.
struct Keyed<K: Hash + Eq + Clone, C: Clock> {
    limiter: RateLimiter<K, SharedStore<K>, C, NoOpMiddleware<C::Instant>>,
    store: SharedStore<K>,
    max_keys: u64,
    /// When idle keys were last reclaimed (on the limiter's clock); `None` before the first.
    /// Also paces the full-store warning: at most one per reclamation.
    last_prune: Mutex<Option<C::Instant>>,
    signals: LimiterSignals,
    /// The limiter's name in the full-store warning (`per-order`, `per-caller`).
    name: &'static str,
}
impl<K: Hash + Eq + Clone, C: Clock> Keyed<K, C> {
    fn new(
        per_minute: NonZeroU32,
        max_keys: u64,
        clock: C,
        signals: LimiterSignals,
        name: &'static str,
    ) -> Self {
        let store = SharedStore::default();
        Self {
            limiter: RateLimiter::new(Quota::per_minute(per_minute), store.clone(), clock),
            store,
            max_keys,
            last_prune: Mutex::new(None),
            signals,
            name,
        }
    }

    fn tracked(&self) -> u64 {
        u64::try_from(self.store.0.len()).unwrap_or(u64::MAX)
    }

    fn is_full(&self) -> bool {
        self.tracked() >= self.max_keys
    }

    /// Reclaim fully replenished keys, at most once per [`PRUNE_INTERVAL`]; `true` when this
    /// call reclaimed.
    fn prune_if_due(&self) -> bool {
        let now = self.limiter.clock().now();
        let mut last = self.last_prune.lock();
        let due = last.is_none_or(|at| now.duration_since(at) >= Nanos::from(PRUNE_INTERVAL));
        if due {
            *last = Some(now);
            drop(last);
            self.limiter.retain_recent();
        }
        due
    }

    /// Count a refusal by kind.
    fn refuse(&self, rejection: Rejection) -> Rejection {
        self.signals
            .rejections
            .add(1, &[KeyValue::new("kind", rejection.kind())]);
        rejection
    }

    /// Count one request against `key`; refuse with `limited(retry_after)` when its budget is
    /// exhausted, or with `full` when the key is new and the bound is reached with no idle key
    /// to reclaim. An already-tracked key is never refused by the bound: it is measured against
    /// its own bucket, as under the gateway's `max_keys`. Never touches storage.
    fn admit(
        &self,
        key: &K,
        limited: fn(u64) -> Rejection,
        full: Rejection,
    ) -> Result<(), Rejection> {
        let tracked = self.tracked();
        self.signals
            .tracked_keys
            .record(tracked, &self.signals.gauge_attributes);
        if tracked >= self.max_keys && !self.store.0.contains_key(key) {
            let pruned = self.prune_if_due();
            // Re-check after reclamation (or after a concurrent request tracked this key). The
            // bound is approximate under concurrency, like the gateway's: two simultaneous new
            // keys may both take the last slot, never more than that per request.
            if self.is_full() && !self.store.0.contains_key(key) {
                if pruned {
                    // Paced by the reclamation interval; the key is not logged.
                    tracing::warn!(
                        target: "orders.rest.throttle",
                        limiter = self.name,
                        tracked_keys = self.tracked(),
                        max_keys = self.max_keys,
                        "bss-orders: limiter key store is full; new keys are refused"
                    );
                }
                return Err(self.refuse(full));
            }
        }
        match self.limiter.check_key(key) {
            Ok(()) => Ok(()),
            Err(not_until) => {
                let wait = not_until.wait_time_from(self.limiter.clock().now());
                // Round up: a positive sub-second wait must not become 0.
                let retry_after_seconds = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
                Err(self.refuse(limited(retry_after_seconds)))
            }
        }
    }
}

/// The gear-local `(subject_id, orderId)` limiter.
///
/// Generic over the `governor` clock so the reclamation and refill behaviour is testable on a
/// fake clock; the runtime uses [`DefaultClock`].
pub struct PerOrderLimiter<C: Clock = DefaultClock> {
    keyed: Keyed<Key, C>,
    settings: ThrottleSettings,
}
impl<C: Clock> std::fmt::Debug for PerOrderLimiter<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerOrderLimiter")
            .field("settings", &self.settings)
            .field("tracked_keys", &self.keyed.store.0.len())
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
        Arc::new(Self {
            keyed: Keyed::new(
                settings.per_order_per_minute,
                settings.per_order_max_keys,
                clock,
                LimiterSignals::new(),
                "per-order",
            ),
            settings,
        })
    }

    #[must_use]
    pub fn settings(&self) -> ThrottleSettings {
        self.settings
    }

    /// Count one request against `(subject_id, order_id)`; refuse when the pair's budget is
    /// exhausted, or when the pair is new and the key bound is reached with no idle key to
    /// reclaim. An already-tracked pair is never refused by the bound: it is measured against
    /// its own bucket, as under the gateway's `max_keys`. Never touches storage.
    ///
    /// # Errors
    /// The rejection to answer.
    pub fn admit(&self, subject_id: Uuid, order_id: Uuid) -> Result<(), Rejection> {
        self.keyed.admit(
            &(subject_id, order_id),
            |retry_after_seconds| Rejection::PerOrderLimit {
                retry_after_seconds,
            },
            Rejection::MaxKeys {
                retry_after_seconds: 60,
            },
        )
    }

    /// Number of tracked keys (tests; the gauge reads the same count).
    #[must_use]
    pub fn tracked_keys(&self) -> usize {
        self.keyed.store.0.len()
    }
}

/// The default bound on tracked callers of one [`PerCallerLimiter`] (as the per-order store).
pub const PER_CALLER_MAX_KEYS_BASELINE: u64 = 100_000;

/// Validated per-caller limiter settings (`gate.preview_per_caller_per_minute`,
/// `gate.submit_per_caller_per_minute`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerLimitSettings {
    /// Requests per minute per authenticated subject.
    pub per_minute: NonZeroU32,
    /// Bound on tracked subjects.
    pub max_keys: u64,
}

/// The per-caller limiters of the gate. The identity is typed so the `limiter` metric attribute
/// stays bounded to these tokens (D-223 item 3, after the api-gateway throttling middleware).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerLimiter {
    /// The Preview limiter (`gate.preview_per_caller_per_minute`).
    Preview,
    /// The submit limiter (`gate.submit_per_caller_per_minute`).
    Submit,
}
impl CallerLimiter {
    /// The metric label and log name of the limiter.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Submit => "submit",
        }
    }
}

/// A gear-local per-caller limiter keyed by the authenticated `subject_id` (F03 §4.6: Preview
/// is rate limited because its diagnostics write is open to every authorized buyer, D-52). The
/// same keyed token bucket, key bound and signals as [`PerOrderLimiter`]; its tracked-key gauge
/// is `orders_throttle_caller_tracked_keys{limiter}` and its refusals count as `per_caller` or
/// `caller_max_keys`. The subject is never a metric attribute and never logged.
pub struct PerCallerLimiter<C: Clock = DefaultClock> {
    keyed: Keyed<Uuid, C>,
    settings: CallerLimitSettings,
}
impl<C: Clock> std::fmt::Debug for PerCallerLimiter<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerCallerLimiter")
            .field("name", &self.keyed.name)
            .field("settings", &self.settings)
            .field("tracked_keys", &self.keyed.store.0.len())
            .finish_non_exhaustive()
    }
}
impl PerCallerLimiter {
    /// The `which` limiter on the runtime clock.
    #[must_use]
    pub fn new(which: CallerLimiter, settings: CallerLimitSettings) -> Arc<Self> {
        Self::with_clock(which, settings, DefaultClock::default())
    }
}
impl<C: Clock> PerCallerLimiter<C> {
    /// Build the limiter over an explicit clock (the runtime uses [`PerCallerLimiter::new`]).
    #[must_use]
    pub fn with_clock(which: CallerLimiter, settings: CallerLimitSettings, clock: C) -> Arc<Self> {
        let name = which.token();
        Arc::new(Self {
            keyed: Keyed::new(
                settings.per_minute,
                settings.max_keys,
                clock,
                LimiterSignals::named(
                    "orders_throttle_caller_tracked_keys",
                    "Subjects an Orders per-caller limiter tracks",
                )
                .labelled(name),
                name,
            ),
            settings,
        })
    }

    #[must_use]
    pub fn settings(&self) -> CallerLimitSettings {
        self.settings
    }

    /// Count one request of `subject_id`.
    ///
    /// # Errors
    /// [`Rejection::PerCallerLimit`] when the subject's budget is exhausted;
    /// [`Rejection::CallerMaxKeys`] when the subject is new and the key bound is reached.
    pub fn admit(&self, subject_id: Uuid) -> Result<(), Rejection> {
        self.keyed.admit(
            &subject_id,
            |retry_after_seconds| Rejection::PerCallerLimit {
                retry_after_seconds,
            },
            Rejection::CallerMaxKeys {
                retry_after_seconds: 60,
            },
        )
    }

    /// Number of tracked subjects.
    #[must_use]
    pub fn tracked_keys(&self) -> usize {
        self.keyed.store.0.len()
    }
}

/// The canonical `resource_exhausted` error of a limiter refusal, scoped to the Orders order
/// resource, with the quota violation subject `throttling` and its retry hint: the body of
/// [`rejection_response`], and what the local SDK returns for a per-caller refusal.
#[must_use]
pub fn rejection_error(rejection: Rejection) -> toolkit_canonical_errors::CanonicalError {
    OrdersOrderError::resource_exhausted(rejection.detail())
        .with_quota_violation("throttling", rejection.description())
        .with_quota_violation_retry_after_seconds(rejection.retry_after_seconds())
        .create()
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

    /// The rejection counter's `kind` attribute names the limit, never the key (D-223).
    #[test]
    fn rejection_kinds_are_the_bounded_metric_attribute() {
        assert_eq!(
            Rejection::PerOrderLimit {
                retry_after_seconds: 1
            }
            .kind(),
            "per_order"
        );
        assert_eq!(
            Rejection::MaxKeys {
                retry_after_seconds: 60
            }
            .kind(),
            "max_keys"
        );
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

    /// The per-caller budget is keyed by the subject alone and refuses as `per_caller`.
    #[test]
    fn the_per_caller_budget_is_keyed_by_subject() {
        let clock = FakeRelativeClock::default();
        let limiter = PerCallerLimiter::with_clock(
            CallerLimiter::Preview,
            CallerLimitSettings {
                per_minute: NonZeroU32::new(2).unwrap(),
                max_keys: 1,
            },
            clock.clone(),
        );
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        assert!(limiter.admit(a).is_ok());
        assert!(limiter.admit(a).is_ok());
        let rejected = limiter.admit(a).unwrap_err();
        assert_eq!(
            rejected,
            Rejection::PerCallerLimit {
                retry_after_seconds: 30
            }
        );
        assert_eq!(rejected.kind(), "per_caller");
        // The store is full: a new subject is refused by the bound, not by a budget.
        assert_eq!(
            limiter.admit(b).unwrap_err(),
            Rejection::CallerMaxKeys {
                retry_after_seconds: 60
            }
        );
        assert_eq!(limiter.tracked_keys(), 1);
        clock.advance(Duration::from_secs(30));
        assert!(limiter.admit(a).is_ok());
    }
}
