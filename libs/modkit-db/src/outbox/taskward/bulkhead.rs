use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;

/// Concurrency gating strategy for the worker bulkhead.
///
/// - `Unlimited`: no semaphore — worker runs ungated.
/// - `Fixed`: classic single-semaphore gate.
/// - `Tiered`: two-tier gate — tries `shared` first (biased), falls back to
///   `guaranteed`. This lets high-priority workers (sequencers) steal shared
///   permits when low-priority tasks (vacuum) are idle.
#[derive(Clone, Debug, Default)]
pub enum ConcurrencyLimit {
    /// No concurrency gating.
    #[default]
    Unlimited,
    /// Fixed concurrency limit via a single semaphore.
    Fixed(Arc<Semaphore>),
    /// Two-tier: try shared first (biased), fall back to guaranteed.
    Tiered {
        guaranteed: Arc<Semaphore>,
        shared: Arc<Semaphore>,
    },
}

/// Exponential backoff parameters.
#[derive(Debug, Clone)]
pub struct BackoffConfig {
    /// First backoff interval after the initial failure.
    pub initial: Duration,
    /// Upper bound — escalation never exceeds this.
    pub max: Duration,
    /// Growth factor per consecutive failure (e.g. 2.0 for doubling).
    pub multiplier: f64,
    /// Jitter factor (0.0–1.0). Scales each interval by a deterministic
    /// factor in `[1.0 - jitter, 1.0 + jitter]`. Default: 0.3 (±30%).
    pub jitter: f64,
}

impl BackoffConfig {
    /// Validated constructor. Panics on invalid inputs.
    ///
    /// # Panics
    /// - `multiplier < 1.0`
    /// - `initial > max`
    #[must_use]
    pub fn new(initial: Duration, max: Duration, multiplier: f64) -> Self {
        assert!(
            multiplier >= 1.0,
            "BackoffConfig: multiplier must be >= 1.0, got {multiplier}"
        );
        assert!(
            initial <= max,
            "BackoffConfig: initial ({initial:?}) must be <= max ({max:?})"
        );
        Self {
            initial,
            max,
            multiplier,
            jitter: 0.3,
        }
    }
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            initial: Duration::from_millis(100),
            max: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: 0.3,
        }
    }
}

/// Construction parameters for [`Bulkhead`].
pub struct BulkheadConfig {
    /// Concurrency gating mode.
    pub semaphore: ConcurrencyLimit,
    /// Backoff strategy applied on action errors.
    pub backoff: BackoffConfig,
}

/// Fused concurrency gate + error-driven backoff.
///
/// The worker loop calls [`acquire`] before every `execute()` and
/// [`escalate`]/[`reset`] after, based on the action result.
/// [`min_interval`] returns the current error-backoff floor.
pub struct Bulkhead {
    name: String,
    semaphore: ConcurrencyLimit,
    backoff: BackoffConfig,
    consecutive_failures: u32,
    current_interval: Duration,
}

impl Bulkhead {
    #[must_use]
    pub fn new(name: impl Into<String>, config: BulkheadConfig) -> Self {
        Self {
            name: name.into(),
            semaphore: config.semaphore,
            backoff: config.backoff,
            consecutive_failures: 0,
            current_interval: Duration::ZERO,
        }
    }

    /// Record a failure — escalate the backoff floor.
    pub fn escalate(&mut self) {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let exponent = self.consecutive_failures.saturating_sub(1);
        let exp = self
            .backoff
            .multiplier
            .powi(i32::try_from(exponent).unwrap_or(i32::MAX));
        let raw = self.backoff.initial.mul_f64(exp);
        let capped = raw.min(self.backoff.max);
        self.current_interval = self.apply_jitter(capped);
    }

    /// Record a success — reset backoff state.
    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.current_interval = Duration::ZERO;
    }

    /// Current error-backoff floor. Returns `Duration::ZERO` when healthy
    /// (after `reset()`), escalates on consecutive failures.
    #[must_use]
    pub fn min_interval(&self) -> Duration {
        self.current_interval
    }

    /// Current consecutive failure count.
    #[must_use]
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Acquire a semaphore permit (cancel-aware).
    ///
    /// Returns `Some(permit)` on success — the caller must hold the permit
    /// for the duration of the gated work. Returns `None` if cancelled or
    /// semaphore closed. When no semaphore is configured, returns
    /// `Some(None)` (the outer Option signals success, the inner signals
    /// no permit to hold).
    ///
    /// In `Priority` mode, the worker tries `shared` first (biased select),
    /// falling back to `guaranteed`. This lets high-priority workers steal
    /// shared permits when low-priority tasks are idle.
    pub async fn acquire(
        &self,
        cancel: &CancellationToken,
    ) -> Option<Option<OwnedSemaphorePermit>> {
        match &self.semaphore {
            ConcurrencyLimit::Unlimited => Some(None),
            ConcurrencyLimit::Fixed(sem) => {
                if cancel.is_cancelled() {
                    return None;
                }
                tokio::select! {
                    () = cancel.cancelled() => None,
                    result = sem.clone().acquire_owned() => result.ok().map(Some),
                }
            }
            ConcurrencyLimit::Tiered { guaranteed, shared } => {
                if cancel.is_cancelled() {
                    return None;
                }
                // biased: prefer shared permit first. This implements priority
                // preemption — Tiered workers (sequencers) steal shared permits
                // from Fixed workers (vacuum) when both compete. Vacuum only
                // runs when sequencers are idle or using their guaranteed permits.
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => None,
                    result = shared.clone().acquire_owned() => result.ok().map(Some),
                    result = guaranteed.clone().acquire_owned() => result.ok().map(Some),
                }
            }
        }
    }

    /// Apply deterministic jitter to a duration.
    ///
    /// Uses a hash of `(name, consecutive_failures)` to produce a factor
    /// in `[1.0 - jitter, 1.0 + jitter]`.
    fn apply_jitter(&self, interval: Duration) -> Duration {
        let jitter = self.backoff.jitter;
        if jitter == 0.0 || interval.is_zero() {
            return interval;
        }
        let hash = xxhash_rust::xxh3::xxh3_64(
            format!("{}:{}", self.name, self.consecutive_failures).as_bytes(),
        );
        // Map hash to [0.0, 1.0)
        #[allow(clippy::cast_precision_loss)]
        let fraction = (hash as f64) / (u64::MAX as f64);
        // Scale to [1.0 - jitter, 1.0 + jitter]
        let factor = 1.0 - jitter + fraction * 2.0 * jitter;
        let jittered = interval.mul_f64(factor);
        // Never exceed max
        jittered.min(self.backoff.max)
    }
}

impl Default for Bulkhead {
    fn default() -> Self {
        Self {
            name: String::new(),
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial: Duration::ZERO,
                max: Duration::ZERO,
                multiplier: 1.0,
                jitter: 0.0,
            },
            consecutive_failures: 0,
            current_interval: Duration::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(initial: Duration, max: Duration, multiplier: f64) -> BulkheadConfig {
        BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial,
                max,
                multiplier,
                jitter: 0.0,
            },
        }
    }

    // -- BackoffConfig validation tests --

    #[test]
    fn new_valid_config() {
        let cfg = BackoffConfig::new(Duration::from_secs(1), Duration::from_secs(60), 2.0);
        assert_eq!(cfg.initial, Duration::from_secs(1));
        assert_eq!(cfg.max, Duration::from_secs(60));
        assert!((cfg.multiplier - 2.0).abs() < f64::EPSILON);
        assert!((cfg.jitter - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn default_backoff_config() {
        let cfg = BackoffConfig::default();
        assert_eq!(cfg.initial, Duration::from_millis(100));
        assert_eq!(cfg.max, Duration::from_secs(30));
        assert!((cfg.multiplier - 2.0).abs() < f64::EPSILON);
        assert!((cfg.jitter - 0.3).abs() < f64::EPSILON);
    }

    #[test]
    fn struct_update_syntax_with_default() {
        let cfg = BackoffConfig {
            max: Duration::from_secs(60),
            ..Default::default()
        };
        assert_eq!(cfg.initial, Duration::from_millis(100));
        assert_eq!(cfg.max, Duration::from_secs(60));
        assert!((cfg.multiplier - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    #[should_panic(expected = "multiplier must be >= 1.0")]
    fn new_panics_on_low_multiplier() {
        let _cfg = BackoffConfig::new(Duration::from_secs(1), Duration::from_secs(60), 0.5);
    }

    #[test]
    #[should_panic(expected = "initial")]
    fn new_panics_on_initial_exceeds_max() {
        let _cfg = BackoffConfig::new(Duration::from_secs(60), Duration::from_secs(1), 2.0);
    }

    // -- Escalation tests (jitter=0.0 for deterministic) --

    #[test]
    fn escalate_first() {
        let mut bh = Bulkhead::new(
            "test",
            test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
        );
        bh.escalate();
        assert_eq!(bh.consecutive_failures(), 1);
        assert_eq!(bh.min_interval(), Duration::from_secs(1));
    }

    #[test]
    fn escalate_exponential() {
        let mut bh = Bulkhead::new(
            "test",
            test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
        );
        let expected = [1, 2, 4, 8];
        for &exp in &expected {
            bh.escalate();
            assert_eq!(bh.min_interval(), Duration::from_secs(exp));
        }
    }

    #[test]
    fn escalate_caps_at_max() {
        let mut bh = Bulkhead::new(
            "test",
            test_config(Duration::from_secs(1), Duration::from_secs(30), 2.0),
        );
        for _ in 0..10 {
            bh.escalate();
        }
        assert_eq!(bh.min_interval(), Duration::from_secs(30));
    }

    #[test]
    fn escalate_linear() {
        let mut bh = Bulkhead::new(
            "test",
            test_config(Duration::from_secs(5), Duration::from_secs(5), 1.0),
        );
        for _ in 0..3 {
            bh.escalate();
            assert_eq!(bh.min_interval(), Duration::from_secs(5));
        }
    }

    // -- Reset tests --

    #[test]
    fn reset_clears_state() {
        let mut bh = Bulkhead::new(
            "test",
            test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
        );
        for _ in 0..3 {
            bh.escalate();
        }
        bh.reset();
        assert_eq!(bh.consecutive_failures(), 0);
        assert_eq!(bh.min_interval(), Duration::ZERO);
    }

    #[test]
    fn reset_on_fresh() {
        let mut bh = Bulkhead::default();
        bh.reset();
        assert_eq!(bh.consecutive_failures(), 0);
        assert_eq!(bh.min_interval(), Duration::ZERO);
    }

    #[test]
    fn default_bulkhead() {
        let bh = Bulkhead::default();
        assert_eq!(bh.min_interval(), Duration::ZERO);
        assert!(matches!(bh.semaphore, ConcurrencyLimit::Unlimited));
    }

    // -- consecutive_failures getter tests --

    #[test]
    fn consecutive_failures_fresh_is_zero() {
        let bh = Bulkhead::default();
        assert_eq!(bh.consecutive_failures(), 0);
    }

    #[test]
    fn consecutive_failures_after_escalation() {
        let mut bh = Bulkhead::new(
            "test",
            test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
        );
        bh.escalate();
        bh.escalate();
        bh.escalate();
        assert_eq!(bh.consecutive_failures(), 3);
    }

    #[test]
    fn consecutive_failures_after_reset() {
        let mut bh = Bulkhead::new(
            "test",
            test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
        );
        bh.escalate();
        bh.escalate();
        bh.reset();
        assert_eq!(bh.consecutive_failures(), 0);
    }

    // -- Jitter tests --

    #[test]
    fn jitter_varies_interval_within_bounds() {
        let config = BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial: Duration::from_secs(10),
                max: Duration::from_secs(60),
                multiplier: 1.0,
                jitter: 0.1,
            },
        };
        let mut bh = Bulkhead::new("test-worker", config);
        bh.escalate();
        let interval = bh.min_interval();
        // ±10% of 10s = [9s, 11s]
        assert!(
            interval >= Duration::from_secs(9) && interval <= Duration::from_secs(11),
            "interval {interval:?} should be in [9s, 11s]"
        );
    }

    #[test]
    fn zero_jitter_is_deterministic() {
        let mut bh = Bulkhead::new(
            "test",
            test_config(Duration::from_secs(1), Duration::from_secs(60), 2.0),
        );
        bh.escalate();
        assert_eq!(bh.min_interval(), Duration::from_secs(1));
        bh.escalate();
        assert_eq!(bh.min_interval(), Duration::from_secs(2));
    }

    #[test]
    fn jitter_does_not_exceed_max() {
        let config = BulkheadConfig {
            semaphore: ConcurrencyLimit::Unlimited,
            backoff: BackoffConfig {
                initial: Duration::from_secs(28),
                max: Duration::from_secs(30),
                multiplier: 1.0,
                jitter: 0.2,
            },
        };
        let mut bh = Bulkhead::new("test-worker", config);
        // Even with +20% jitter on 28s = 33.6s, it should be capped at 30s
        for _ in 0..10 {
            bh.escalate();
            assert!(
                bh.min_interval() <= Duration::from_secs(30),
                "interval {:?} should not exceed max 30s",
                bh.min_interval()
            );
            bh.reset();
        }
    }

    // -- Acquire tests --

    #[tokio::test]
    async fn acquire_with_permit() {
        let sem = Arc::new(Semaphore::new(1));
        let bh = Bulkhead::new(
            "test",
            BulkheadConfig {
                semaphore: ConcurrencyLimit::Fixed(sem),
                backoff: BackoffConfig {
                    initial: Duration::ZERO,
                    max: Duration::ZERO,
                    multiplier: 1.0,
                    jitter: 0.0,
                },
            },
        );
        let cancel = CancellationToken::new();
        assert!(bh.acquire(&cancel).await.is_some());
    }

    #[tokio::test]
    async fn acquire_cancel_during_wait() {
        let sem = Arc::new(Semaphore::new(0));
        let bh = Bulkhead::new(
            "test",
            BulkheadConfig {
                semaphore: ConcurrencyLimit::Fixed(sem),
                backoff: BackoffConfig {
                    initial: Duration::ZERO,
                    max: Duration::ZERO,
                    multiplier: 1.0,
                    jitter: 0.0,
                },
            },
        );
        let cancel = CancellationToken::new();
        let cancel_c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel_c.cancel();
        });
        assert!(bh.acquire(&cancel).await.is_none());
    }

    #[tokio::test]
    async fn acquire_already_cancelled() {
        let sem = Arc::new(Semaphore::new(1));
        let bh = Bulkhead::new(
            "test",
            BulkheadConfig {
                semaphore: ConcurrencyLimit::Fixed(sem),
                backoff: BackoffConfig {
                    initial: Duration::ZERO,
                    max: Duration::ZERO,
                    multiplier: 1.0,
                    jitter: 0.0,
                },
            },
        );
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(bh.acquire(&cancel).await.is_none());
    }

    #[tokio::test]
    async fn acquire_closed_semaphore() {
        let sem = Arc::new(Semaphore::new(1));
        sem.close();
        let bh = Bulkhead::new(
            "test",
            BulkheadConfig {
                semaphore: ConcurrencyLimit::Fixed(sem),
                backoff: BackoffConfig {
                    initial: Duration::ZERO,
                    max: Duration::ZERO,
                    multiplier: 1.0,
                    jitter: 0.0,
                },
            },
        );
        let cancel = CancellationToken::new();
        assert!(bh.acquire(&cancel).await.is_none());
    }

    #[tokio::test]
    async fn acquire_no_semaphore() {
        let bh = Bulkhead::default();
        let cancel = CancellationToken::new();
        assert!(bh.acquire(&cancel).await.is_some());
    }

    // -- Priority mode tests --

    fn priority_bulkhead(guaranteed: Arc<Semaphore>, shared: Arc<Semaphore>) -> Bulkhead {
        Bulkhead::new(
            "test",
            BulkheadConfig {
                semaphore: ConcurrencyLimit::Tiered { guaranteed, shared },
                backoff: BackoffConfig {
                    initial: Duration::ZERO,
                    max: Duration::ZERO,
                    multiplier: 1.0,
                    jitter: 0.0,
                },
            },
        )
    }

    #[tokio::test]
    async fn priority_prefers_shared_when_both_available() {
        let guaranteed = Arc::new(Semaphore::new(1));
        let shared = Arc::new(Semaphore::new(1));
        let bh = priority_bulkhead(Arc::clone(&guaranteed), Arc::clone(&shared));
        let cancel = CancellationToken::new();

        let permit = bh.acquire(&cancel).await;
        assert!(permit.is_some());
        // shared should have been consumed (biased select prefers it)
        assert_eq!(shared.available_permits(), 0);
        assert_eq!(guaranteed.available_permits(), 1);
    }

    #[tokio::test]
    async fn priority_falls_back_to_guaranteed_when_shared_exhausted() {
        let guaranteed = Arc::new(Semaphore::new(1));
        let shared = Arc::new(Semaphore::new(0));
        let bh = priority_bulkhead(Arc::clone(&guaranteed), Arc::clone(&shared));
        let cancel = CancellationToken::new();

        let permit = bh.acquire(&cancel).await;
        assert!(permit.is_some());
        assert_eq!(guaranteed.available_permits(), 0);
    }

    #[tokio::test]
    async fn priority_acquires_shared_when_guaranteed_exhausted() {
        let guaranteed = Arc::new(Semaphore::new(0));
        let shared = Arc::new(Semaphore::new(1));
        let bh = priority_bulkhead(Arc::clone(&guaranteed), Arc::clone(&shared));
        let cancel = CancellationToken::new();

        let permit = bh.acquire(&cancel).await;
        assert!(permit.is_some());
        assert_eq!(shared.available_permits(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn priority_cancel_during_wait() {
        let guaranteed = Arc::new(Semaphore::new(0));
        let shared = Arc::new(Semaphore::new(0));
        let bh = priority_bulkhead(guaranteed, shared);
        let cancel = CancellationToken::new();
        let cancel_c = cancel.clone();

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            cancel_c.cancel();
        });
        assert!(bh.acquire(&cancel).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn priority_blocks_when_neither_available() {
        let guaranteed = Arc::new(Semaphore::new(0));
        let shared = Arc::new(Semaphore::new(0));
        let bh = priority_bulkhead(Arc::clone(&guaranteed), Arc::clone(&shared));
        let cancel = CancellationToken::new();

        // Release a guaranteed permit after a short delay
        let g = Arc::clone(&guaranteed);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            g.add_permits(1);
        });

        let permit = tokio::time::timeout(Duration::from_millis(100), bh.acquire(&cancel)).await;
        assert!(permit.is_ok());
        assert!(permit.unwrap().is_some());
    }
}
