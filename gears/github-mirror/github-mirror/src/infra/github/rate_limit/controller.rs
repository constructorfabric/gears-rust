//! Per-token [`RateLimitController`]: tracks `remaining`, `reset_at`, `soft_cap`,
//! `backoff_until` and `in_flight` counts; implements request gating and AIMD
//! adaptive concurrency.
//!
//! One controller is created per unique token and shared across all syncs
//! that use that token via `Arc<RateLimitController>`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Notify};
use tracing::{debug, trace, warn};

use super::RateLimitError;
use super::headers::RateLimitHeaders;
use super::probe::{AuthoritativeQuota, QuotaProbe};

/// Minimum soft-cap floor: never halve below this value.
const SOFT_CAP_MIN: u32 = 1;
/// Default initial soft-cap (max concurrent in-flight per token).
const SOFT_CAP_DEFAULT: u32 = 10;
/// Upper bound on the emergency quota reserve withheld from callers.
///
/// The reserve is nominally ~10% of the hourly quota, but on a large quota
/// (e.g. the 5000-call core pool) a flat 10% strands 500 usable calls and forces
/// a full sleep-until-reset the moment `remaining` dips below it. Capping the
/// reserve keeps a small safety buffer against hard `403`s while letting the
/// sync keep working down to the last few dozen calls before pausing.
const RESERVE_MAX: u32 = 50;

/// Minimum interval between authoritative `GET /rate_limit` reconciliation
/// probes. While many tasks may park simultaneously when the inline header
/// reports exhaustion, the authoritative result is cached for this window so
/// the free endpoint is hit at most once per interval.
const PROBE_COOLDOWN: Duration = Duration::from_secs(5);

/// Mutable state guarded by a `Mutex`.
#[derive(Debug)]
struct Inner {
    /// API calls remaining in the current window.
    remaining: u32,
    /// When the current rate-limit window resets.
    reset_at: Option<DateTime<Utc>>,
    /// Park callers until this time (set on 429 / secondary rate limit).
    backoff_until: Option<DateTime<Utc>>,
    /// Maximum concurrent in-flight requests for this token (AIMD-controlled).
    soft_cap: u32,
    /// Total hourly quota for reference.
    limit: u32,
    /// Most recent authoritative `/rate_limit` snapshot, if probed.
    authoritative: Option<AuthoritativeQuota>,
    /// When [`Inner::authoritative`] was captured (for the [`PROBE_COOLDOWN`]).
    authoritative_at: Option<Instant>,
}

impl Default for Inner {
    fn default() -> Self {
        Self {
            remaining: u32::MAX,
            reset_at: None,
            backoff_until: None,
            soft_cap: SOFT_CAP_DEFAULT,
            limit: 5000,
            authoritative: None,
            authoritative_at: None,
        }
    }
}

/// Immutable snapshot of the gating inputs taken under one lock acquisition,
/// so [`RateLimitController::admit`] evaluates each gate without re-locking.
#[derive(Debug, Clone, Copy)]
struct GateSnapshot {
    /// Maximum concurrent in-flight requests for this token.
    soft_cap: u32,
    /// Active backoff deadline, if any.
    backoff_until: Option<DateTime<Utc>>,
    /// API calls remaining in the current window.
    remaining: u32,
    /// When the current rate-limit window resets.
    reset_at: Option<DateTime<Utc>>,
    /// Emergency reserve (~10% of the hourly quota, capped at [`RESERVE_MAX`])
    /// withheld from callers.
    reserve: u32,
}

/// Outcome of the authoritative-quota reconciliation in
/// [`RateLimitController::reconcile_quota`].
#[derive(Debug, Clone, Copy)]
enum QuotaDecision {
    /// Quota actually remains (inline header was an over-count); proceed.
    Proceed,
    /// Quota is genuinely spent; pause until the carried reset window (if any).
    Pause(Option<DateTime<Utc>>),
}

/// Outcome of a single [`RateLimitController::admit_attempt`] iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdmitStep {
    /// A gate parked the caller; admission must be re-evaluated.
    Retry,
    /// All gates are open; the caller may proceed.
    Admit,
}

/// Per-token rate-limit controller.
///
/// Shared across syncs via `Arc`.
#[derive(Debug)]
pub struct RateLimitController {
    inner: Mutex<Inner>,
    /// Count of currently in-flight requests for this token.
    in_flight: AtomicU32,
    /// Notified when a request slot becomes available (in-flight decremented
    /// or backoff expires).
    notify: Arc<Notify>,
    /// Authoritative `/rate_limit` reconciliation hook, injected once by the
    /// HTTP layer via [`Self::set_quota_probe`]. Absent in contexts that have no
    /// client (e.g. one-shot quota queries), in which case the gate falls back
    /// to inline headers alone.
    probe: OnceLock<Arc<dyn QuotaProbe>>,
    /// Serializes reconciliation probes so a burst of parked tasks issues at
    /// most one in-flight `/rate_limit` call instead of a thundering herd.
    probe_gate: Mutex<()>,
}

impl Default for RateLimitController {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimitController {
    /// Create a new controller with default limits.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            in_flight: AtomicU32::new(0),
            notify: Arc::new(Notify::new()),
            probe: OnceLock::new(),
            probe_gate: Mutex::new(()),
        }
    }

    /// Inject the authoritative `/rate_limit` reconciliation probe.
    ///
    /// Called once by the HTTP layer after the GitHub client is built. The probe
    /// is held behind a [`OnceLock`]; a second call is ignored (the controller
    /// keeps the first probe). Avoids a reference cycle: the probe holds only a
    /// `reqwest` client and the auth header, never the controller.
    pub fn set_quota_probe(&self, probe: Arc<dyn QuotaProbe>) {
        if self.probe.set(probe).is_err() {
            debug!("quota probe already set; keeping the first one");
        }
    }

    /// Create a controller with a specific initial soft-cap and hourly quota.
    ///
    /// # Panics
    /// Never panics; the inner `Mutex` is uncontested at construction time.
    #[must_use]
    pub fn with_limits(soft_cap: u32, limit: u32) -> Self {
        let ctrl = Self::new();
        #[allow(
            clippy::expect_used,
            reason = "Mutex uncontested at construction; try_lock is infallible here"
        )]
        let mut g = ctrl
            .inner
            .try_lock()
            .expect("Mutex uncontested at construction");
        g.soft_cap = soft_cap.max(SOFT_CAP_MIN);
        g.limit = limit;
        drop(g);
        ctrl
    }

    /// Admit an outgoing request for this token.
    ///
    /// Parks the calling task until both conditions are met:
    /// 1. `backoff_until` has passed (if set).
    /// 2. `in_flight < soft_cap` AND effective `remaining > reserve`.
    ///
    /// The caller **must** call [`Self::release`] after the response is
    /// received (success or error). Returns whether the caller was parked.
    ///
    /// # Errors
    /// Returns [`RateLimitError::BudgetExhausted`] if the quota is spent and no
    /// future reset window is known to wait for.
    pub async fn admit(&self) -> Result<bool, RateLimitError> {
        let mut parked = false;
        while self.admit_attempt().await? == AdmitStep::Retry {
            parked = true;
        }
        self.in_flight.fetch_add(1, Ordering::AcqRel);
        debug!(
            in_flight = self.in_flight.load(Ordering::Relaxed),
            "request admitted"
        );
        Ok(parked)
    }

    /// One admission attempt: evaluate every gate once. Returns
    /// [`AdmitStep::Retry`] if any gate parked the caller (admission must be
    /// re-evaluated), or [`AdmitStep::Admit`] if all gates are open.
    ///
    /// # Errors
    /// Propagates [`RateLimitError::BudgetExhausted`] from the quota gate.
    async fn admit_attempt(&self) -> Result<AdmitStep, RateLimitError> {
        let gate = self.gate_snapshot().await;

        if self.honour_backoff(gate.backoff_until).await {
            return Ok(AdmitStep::Retry);
        }
        if self
            .await_quota(gate.remaining, gate.reset_at, gate.reserve)
            .await?
        {
            return Ok(AdmitStep::Retry);
        }
        if self.await_slot(gate.soft_cap).await {
            return Ok(AdmitStep::Retry);
        }
        Ok(AdmitStep::Admit)
    }

    /// Snapshot the gating inputs under a single lock acquisition.
    async fn gate_snapshot(&self) -> GateSnapshot {
        let g = self.inner.lock().await;
        #[allow(
            clippy::integer_division,
            reason = "intentional 10% reserve approximation"
        )]
        let reserve = (g.limit / 10).min(RESERVE_MAX);
        let snapshot = GateSnapshot {
            soft_cap: g.soft_cap,
            backoff_until: g.backoff_until,
            remaining: g.remaining,
            reset_at: g.reset_at,
            reserve,
        };
        drop(g);
        trace!(
            soft_cap = snapshot.soft_cap,
            backoff_until = ?snapshot.backoff_until,
            remaining = snapshot.remaining,
            reset_at = ?snapshot.reset_at,
            reserve = snapshot.reserve,
            in_flight = self.in_flight.load(Ordering::Relaxed),
            "rate-limit gate snapshot"
        );
        snapshot
    }

    pub async fn backing_off(&self) -> bool {
        self.inner
            .lock()
            .await
            .backoff_until
            .is_some_and(|until| until > Utc::now())
    }

    /// Gate 1: if a backoff window is active, park until it elapses. Returns
    /// `true` when the caller parked (and should re-evaluate), `false` otherwise.
    async fn honour_backoff(&self, backoff_until: Option<DateTime<Utc>>) -> bool {
        let Some(until) = backoff_until else {
            return false;
        };
        let now = Utc::now();
        if now >= until {
            return false;
        }
        let delay = (until - now).to_std().unwrap_or_default();
        warn!(
            backoff_until = %until,
            now = %now,
            delay_secs = delay.as_secs(),
            "rate-limit backoff active; parking"
        );
        tokio::time::sleep(delay).await;
        trace!(backoff_until = %until, "rate-limit backoff sleep complete");
        true
    }

    /// Gate 2: enforce the quota (minus the emergency reserve). Returns `true`
    /// when the caller parked (reconciled or slept) and should re-evaluate.
    ///
    /// The inline `X-RateLimit-Remaining` header is unreliable under high
    /// concurrency: GitHub decrements each serving node's view optimistically,
    /// so a burst can drive the inline `remaining` far below the reconciled,
    /// authoritative count it actually enforces. Before committing to a long
    /// sleep-until-reset, this gate reconciles against the free `GET /rate_limit`
    /// endpoint (when a [`QuotaProbe`] is wired); it only sleeps when the
    /// authoritative quota also confirms exhaustion.
    ///
    /// # Errors
    /// Returns [`RateLimitError::BudgetExhausted`] only when quota appears spent
    /// and no reset window is known (and reconciliation could not help).
    async fn await_quota(
        &self,
        remaining: u32,
        reset_at: Option<DateTime<Utc>>,
        reserve: u32,
    ) -> Result<bool, RateLimitError> {
        if remaining.saturating_sub(reserve) > 0 {
            return Ok(false);
        }
        let reset = match self.reconcile_quota(reset_at, reserve).await {
            QuotaDecision::Proceed => return Ok(true),
            QuotaDecision::Pause(reset) => reset,
        };
        match reset {
            Some(reset) if Utc::now() < reset => {
                self.sleep_until_reset(reset, remaining, reserve).await;
                Ok(true)
            }
            Some(_) => {
                self.refill().await;
                Ok(true)
            }
            None => Err(RateLimitError::BudgetExhausted),
        }
    }

    /// Reconcile the inline-reported exhaustion against the authoritative
    /// `/rate_limit` endpoint. Returns [`QuotaDecision::Proceed`] (adopting the
    /// authoritative figures) when quota actually remains, otherwise
    /// [`QuotaDecision::Pause`] carrying the reset window to wait on.
    async fn reconcile_quota(
        &self,
        reset_at: Option<DateTime<Utc>>,
        reserve: u32,
    ) -> QuotaDecision {
        let Some(auth) = self.authoritative_quota().await else {
            return QuotaDecision::Pause(reset_at);
        };
        if auth.remaining.saturating_sub(reserve) == 0 {
            return QuotaDecision::Pause(auth.reset_at.or(reset_at));
        }
        let mut g = self.inner.lock().await;
        g.remaining = auth.remaining;
        g.limit = auth.limit;
        if let Some(reset) = auth.reset_at {
            g.reset_at = Some(reset);
        }
        drop(g);
        debug!(
            authoritative_remaining = auth.remaining,
            reserve,
            "inline rate-limit header indicated exhaustion, but /rate_limit confirms quota; proceeding"
        );
        QuotaDecision::Proceed
    }

    /// Sleep until the quota window resets, then optimistically refill the local
    /// counter and drop the stale authoritative cache so the next admission
    /// proceeds and a real request re-syncs the inline headers, instead of
    /// erroring out on the now-stale low `remaining` against a past reset.
    async fn sleep_until_reset(&self, reset: DateTime<Utc>, remaining: u32, reserve: u32) {
        let now = Utc::now();
        let delay = (reset - now).to_std().unwrap_or_default();
        warn!(
            reset_at = %reset,
            now = %now,
            delay_secs = delay.as_secs(),
            remaining,
            reserve,
            "GitHub rate limit reached; pausing until the quota window resets"
        );
        tokio::time::sleep(delay).await;
        self.refill().await;
        trace!(reset_at = %reset, "quota sleep complete; rechecking admission");
    }

    async fn refill(&self) {
        let mut g = self.inner.lock().await;
        g.remaining = g.limit;
        g.authoritative = None;
        g.authoritative_at = None;
    }

    /// Return the authoritative core quota, refreshing it via the injected
    /// [`QuotaProbe`] at most once per [`PROBE_COOLDOWN`].
    ///
    /// Returns `None` when no probe is wired or the probe fails and no cached
    /// value is available, in which case the caller falls back to inline headers.
    async fn authoritative_quota(&self) -> Option<AuthoritativeQuota> {
        {
            let g = self.inner.lock().await;
            if let (Some(quota), Some(at)) = (g.authoritative, g.authoritative_at)
                && at.elapsed() < PROBE_COOLDOWN
            {
                return Some(quota);
            }
        }
        let probe = self.probe.get()?.clone();
        let Ok(_guard) = self.probe_gate.try_lock() else {
            let g = self.inner.lock().await;
            return g.authoritative;
        };
        {
            let g = self.inner.lock().await;
            if let (Some(quota), Some(at)) = (g.authoritative, g.authoritative_at)
                && at.elapsed() < PROBE_COOLDOWN
            {
                return Some(quota);
            }
        }
        let fetched = probe.fetch_core_quota().await;
        let mut g = self.inner.lock().await;
        if let Some(quota) = fetched {
            g.authoritative = Some(quota);
            g.authoritative_at = Some(Instant::now());
            Some(quota)
        } else {
            g.authoritative
        }
    }

    /// Gate 3: enforce the per-token concurrency soft-cap. Returns `true` when
    /// the cap was reached and the caller parked waiting for a freed slot.
    async fn await_slot(&self, soft_cap: u32) -> bool {
        let current = self.in_flight.load(Ordering::Acquire);
        if current < soft_cap {
            return false;
        }
        trace!(
            in_flight = current,
            soft_cap, "soft-cap reached; waiting for slot"
        );
        self.notify.notified().await;
        trace!(soft_cap, "soft-cap wait complete; rechecking admission");
        true
    }

    /// Release an in-flight slot. Call after each response (or error).
    pub fn release(&self) {
        let prev = self.in_flight.fetch_sub(1, Ordering::AcqRel);
        trace!(in_flight = prev.saturating_sub(1), "request released");
        self.notify.notify_waiters();
    }

    /// Update controller state from response headers.
    ///
    /// Triggers AIMD: on 429 / `retry_after_secs`, halve `soft_cap`; on
    /// success with remaining > 10%, linearly increase toward `max_cap`.
    ///
    /// The budget fields (`remaining` / `limit` / `reset_at`) are only ingested
    /// for the **core** REST resource. GitHub tracks `search` (~30/min) and
    /// `graphql` (a separate point budget) in independent pools whose
    /// `X-RateLimit-*` headers would otherwise clobber the core budget this
    /// controller gates: a `search` response (`limit=30`, reset in ~60s) would
    /// briefly shrink the reserve and reset window. Secondary rate limits
    /// (`429` / `Retry-After`) apply regardless of resource and are always
    /// honoured.
    pub async fn observe(&self, headers: &RateLimitHeaders, http_status: u16, max_cap: u32) {
        let mut g = self.inner.lock().await;
        Self::ingest_core_budget(&mut g, headers);
        if http_status == 429 || headers.retry_after_secs.is_some() {
            Self::back_off(&mut g, headers);
        } else {
            g.backoff_until = None;
            Self::grow_soft_cap(&mut g, max_cap);
        }
        drop(g);
        self.notify.notify_waiters();
    }

    fn ingest_core_budget(g: &mut Inner, headers: &RateLimitHeaders) {
        let is_core = headers
            .resource
            .as_deref()
            .is_none_or(|r| r.eq_ignore_ascii_case("core"));
        if !is_core {
            return;
        }
        if let Some(remaining) = headers.remaining {
            g.remaining = remaining;
        }
        if let Some(reset_at) = headers.reset_at {
            g.reset_at = Some(reset_at);
        }
        if let Some(limit) = headers.limit {
            g.limit = limit;
        }
    }

    fn back_off(g: &mut Inner, headers: &RateLimitHeaders) {
        g.soft_cap = (g.soft_cap >> 1).max(SOFT_CAP_MIN);
        warn!(
            soft_cap = g.soft_cap,
            "AIMD: soft_cap halved due to 429 / Retry-After"
        );
        if let Some(secs) = headers.retry_after_secs {
            let delta = i64::try_from(secs)
                .ok()
                .and_then(chrono::Duration::try_seconds)
                .unwrap_or(chrono::Duration::MAX);
            g.backoff_until = Some(
                Utc::now()
                    .checked_add_signed(delta)
                    .unwrap_or(DateTime::<Utc>::MAX_UTC),
            );
        }
    }

    fn grow_soft_cap(g: &mut Inner, max_cap: u32) {
        #[allow(
            clippy::integer_division,
            reason = "intentional 10% threshold approximation"
        )]
        let healthy_threshold = g.limit / 10;
        if g.remaining > healthy_threshold && g.soft_cap < max_cap {
            g.soft_cap = (g.soft_cap + 1).min(max_cap);
            debug!(soft_cap = g.soft_cap, "AIMD: soft_cap increased");
        }
    }

    /// Snapshot of current state for diagnostics.
    pub async fn snapshot(&self) -> ControllerSnapshot {
        let g = self.inner.lock().await;
        ControllerSnapshot {
            remaining: g.remaining,
            soft_cap: g.soft_cap,
            in_flight: self.in_flight.load(Ordering::Relaxed),
            backoff_until: g.backoff_until,
            reset_at: g.reset_at,
        }
    }
}

/// Read-only snapshot of controller state (for diagnostics / tests).
#[derive(Debug, Clone)]
pub struct ControllerSnapshot {
    /// Remaining quota.
    pub remaining: u32,
    /// Current soft-cap.
    pub soft_cap: u32,
    /// In-flight request count.
    pub in_flight: u32,
    /// Active backoff deadline, if any.
    pub backoff_until: Option<DateTime<Utc>>,
    /// Rate-limit window reset time.
    pub reset_at: Option<DateTime<Utc>>,
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test helpers panic on unexpected errors by design"
)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;

    #[tokio::test]
    async fn admit_and_release_increments_decrement_in_flight() {
        let ctrl = RateLimitController::new();
        ctrl.admit().await.expect("admit");
        assert_eq!(ctrl.in_flight.load(Ordering::Relaxed), 1);
        ctrl.release();
        assert_eq!(ctrl.in_flight.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn observe_429_halves_soft_cap() {
        let ctrl = RateLimitController::with_limits(10, 5000);
        let hdrs = RateLimitHeaders {
            retry_after_secs: Some(1),
            ..Default::default()
        };
        ctrl.observe(&hdrs, 429, 20).await;
        let snap = ctrl.snapshot().await;
        assert_eq!(snap.soft_cap, 5);
        assert!(snap.backoff_until.is_some());
    }

    #[tokio::test]
    async fn backing_off_holds_from_a_retry_after_until_the_next_good_answer() {
        let ctrl = RateLimitController::new();
        assert!(!ctrl.backing_off().await);

        let limited = RateLimitHeaders {
            retry_after_secs: Some(60),
            ..Default::default()
        };
        ctrl.observe(&limited, 429, 20).await;
        assert!(ctrl.backing_off().await);

        ctrl.observe(&RateLimitHeaders::default(), 200, 20).await;
        assert!(!ctrl.backing_off().await);
    }

    #[tokio::test]
    async fn a_backoff_already_past_is_not_backing_off() {
        let ctrl = RateLimitController::new();
        let limited = RateLimitHeaders {
            retry_after_secs: Some(0),
            ..Default::default()
        };
        ctrl.observe(&limited, 429, 20).await;

        assert!(!ctrl.backing_off().await);
    }

    #[tokio::test]
    async fn observe_success_increases_soft_cap() {
        let ctrl = RateLimitController::with_limits(5, 5000);
        let hdrs = RateLimitHeaders {
            limit: Some(5000),
            remaining: Some(4000),
            ..Default::default()
        };
        ctrl.observe(&hdrs, 200, 20).await;
        let snap = ctrl.snapshot().await;
        assert_eq!(snap.soft_cap, 6);
    }

    #[tokio::test]
    async fn observe_success_does_not_exceed_max_cap() {
        let ctrl = RateLimitController::with_limits(20, 5000);
        let hdrs = RateLimitHeaders {
            limit: Some(5000),
            remaining: Some(4000),
            ..Default::default()
        };
        ctrl.observe(&hdrs, 200, 20).await;
        let snap = ctrl.snapshot().await;
        assert_eq!(snap.soft_cap, 20);
    }

    #[tokio::test]
    async fn reserve_is_capped_so_admission_continues_on_large_quota() {
        let ctrl = RateLimitController::with_limits(10, 5000);
        ctrl.observe(
            &RateLimitHeaders {
                limit: Some(5000),
                remaining: Some(461),
                reset_at: Some(Utc::now() + chrono::Duration::minutes(30)),
                resource: Some("core".to_owned()),
                ..Default::default()
            },
            200,
            20,
        )
        .await;
        let snap = ctrl.gate_snapshot().await;
        assert_eq!(snap.reserve, RESERVE_MAX, "reserve capped at RESERVE_MAX");
        assert!(
            snap.remaining.saturating_sub(snap.reserve) > 0,
            "461 calls remain above the capped reserve, so admission proceeds"
        );
    }

    #[tokio::test]
    async fn snapshot_reflects_state() {
        let ctrl = RateLimitController::new();
        let snap = ctrl.snapshot().await;
        assert_eq!(snap.in_flight, 0);
        assert!(snap.backoff_until.is_none());
    }

    #[tokio::test]
    async fn observe_ignores_non_core_budget_headers() {
        let ctrl = RateLimitController::with_limits(10, 5000);
        ctrl.observe(
            &RateLimitHeaders {
                limit: Some(5000),
                remaining: Some(4000),
                reset_at: Utc.timestamp_opt(1_780_728_587, 0).single(),
                resource: Some("core".to_owned()),
                ..Default::default()
            },
            200,
            20,
        )
        .await;
        let before = ctrl.snapshot().await;

        ctrl.observe(
            &RateLimitHeaders {
                limit: Some(30),
                remaining: Some(3),
                reset_at: Utc.timestamp_opt(1_780_725_000, 0).single(),
                resource: Some("search".to_owned()),
                ..Default::default()
            },
            200,
            20,
        )
        .await;
        let after = ctrl.snapshot().await;

        assert_eq!(
            after.remaining, before.remaining,
            "core remaining preserved"
        );
        assert_eq!(
            after.reset_at, before.reset_at,
            "core reset window preserved"
        );
    }

    #[derive(Debug)]
    struct MockProbe {
        quota: AuthoritativeQuota,
        calls: AtomicU32,
    }

    #[async_trait::async_trait]
    impl QuotaProbe for MockProbe {
        async fn fetch_core_quota(&self) -> Option<AuthoritativeQuota> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Some(self.quota)
        }
    }

    fn seed_inline_exhausted(remaining: u32) -> RateLimitHeaders {
        RateLimitHeaders {
            limit: Some(5000),
            remaining: Some(remaining),
            reset_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            resource: Some("core".to_owned()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn reconcile_proceeds_when_authoritative_reports_quota() {
        let ctrl = RateLimitController::with_limits(10, 5000);
        ctrl.observe(&seed_inline_exhausted(10), 200, 20).await;
        let probe = Arc::new(MockProbe {
            quota: AuthoritativeQuota {
                remaining: 2000,
                limit: 5000,
                reset_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            },
            calls: AtomicU32::new(0),
        });
        ctrl.set_quota_probe(probe.clone());

        ctrl.admit()
            .await
            .expect("admit proceeds after reconciliation");
        ctrl.release();

        assert_eq!(ctrl.snapshot().await.remaining, 2000);
        assert_eq!(probe.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn reconcile_uses_cached_authoritative_within_cooldown() {
        let ctrl = RateLimitController::with_limits(10, 5000);
        let probe = Arc::new(MockProbe {
            quota: AuthoritativeQuota {
                remaining: 2000,
                limit: 5000,
                reset_at: Some(Utc::now() + chrono::Duration::minutes(30)),
            },
            calls: AtomicU32::new(0),
        });
        ctrl.set_quota_probe(probe.clone());

        ctrl.observe(&seed_inline_exhausted(10), 200, 20).await;
        ctrl.admit().await.expect("first admit");
        ctrl.release();

        ctrl.observe(&seed_inline_exhausted(8), 200, 20).await;
        ctrl.admit().await.expect("second admit");
        ctrl.release();

        assert_eq!(probe.calls.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn observe_honours_secondary_limit_regardless_of_resource() {
        let ctrl = RateLimitController::with_limits(10, 5000);
        ctrl.observe(
            &RateLimitHeaders {
                resource: Some("search".to_owned()),
                retry_after_secs: Some(1),
                ..Default::default()
            },
            429,
            20,
        )
        .await;
        let snap = ctrl.snapshot().await;
        assert_eq!(snap.soft_cap, 5);
        assert!(snap.backoff_until.is_some());
    }
}
