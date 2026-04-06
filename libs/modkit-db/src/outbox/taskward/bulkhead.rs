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
#[path = "bulkhead_tests.rs"]
mod tests;
