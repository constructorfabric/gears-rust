//! Sliding-window circuit breaker used by the usage-collector domain service.
//!
//! Wraps fallible async calls to the storage plugin so that repeated
//! infrastructure failures stop hammering an unhealthy backend, while
//! caller-induced errors do not trip the breaker.
//!
//! The single entry point is [`CircuitBreaker::execute`], which acquires a
//! permit, invokes the closure, classifies the result, and updates breaker
//! state atomically.

#![deny(clippy::await_holding_lock)]

use std::time::Instant;

use parking_lot::Mutex;
use tracing::{info, warn};
use usage_collector_sdk::UsageCollectorError;

use super::error::DomainError;
use crate::config::CircuitBreakerConfig;

#[derive(Debug)]
enum State {
    Closed,
    Open { opened_at: Instant },
    HalfOpen,
}

#[derive(Debug)]
struct Inner {
    config: CircuitBreakerConfig,
    state: State,
    failure_timestamps: Vec<Instant>,
}

impl Inner {
    /// Atomically check whether the next call may proceed and (if so) update
    /// state for an in-flight probe.
    ///
    /// Returns `Ok(was_probe)` — `true` when the caller is the single
    /// `HalfOpen` probe, `false` for normal `Closed` traffic — or
    /// `Err(DomainError::CircuitOpen)` when the circuit is rejecting calls.
    fn try_acquire(&mut self) -> Result<bool, DomainError> {
        match &self.state {
            State::Closed => Ok(false),
            State::Open { opened_at } => {
                if opened_at.elapsed() >= self.config.recovery_timeout {
                    info!("Circuit breaker transitioning from Open to HalfOpen for probe");
                    self.state = State::HalfOpen;
                    Ok(true)
                } else {
                    Err(DomainError::CircuitOpen)
                }
            }
            // A probe is already in flight; reject everyone else until it completes.
            State::HalfOpen => Err(DomainError::CircuitOpen),
        }
    }

    fn record_failure(&mut self) {
        let now = Instant::now();

        // A failure during the HalfOpen probe must re-open unconditionally:
        // the probe's job is to confirm health, and the threshold/window
        // bookkeeping does not apply (the window was cleared on entry to Open).
        if matches!(self.state, State::HalfOpen) {
            warn!("Circuit breaker re-opening after failed HalfOpen probe");
            self.state = State::Open { opened_at: now };
            self.failure_timestamps.clear();
            return;
        }

        self.failure_timestamps
            .retain(|t| now.duration_since(*t) < self.config.window);
        self.failure_timestamps.push(now);

        let failures_in_window = self.failure_timestamps.len();

        if failures_in_window >= self.config.failure_threshold as usize {
            match self.state {
                State::Closed => {
                    warn!(
                        failures_in_window,
                        threshold = self.config.failure_threshold,
                        "Circuit breaker opening after too many failures within the rolling window"
                    );
                    self.state = State::Open { opened_at: now };
                    self.failure_timestamps.clear();
                }
                // HalfOpen is handled above; Open already at opened_at — resetting
                // would prevent recovery under sustained load.
                State::Open { .. } | State::HalfOpen => {}
            }
        }
    }

    fn record_success(&mut self) {
        match self.state {
            State::HalfOpen => {
                info!(
                    "Circuit breaker transitioning from HalfOpen to Closed after successful probe"
                );
                self.state = State::Closed;
                self.failure_timestamps.clear();
            }
            State::Closed => {
                self.failure_timestamps.clear();
            }
            State::Open { .. } => {
                warn!("Circuit breaker received success while Open; resetting to Closed");
                self.state = State::Closed;
                self.failure_timestamps.clear();
            }
        }
    }
}

/// Sliding-window circuit breaker.
///
/// Opens after `failure_threshold` failures within the rolling `window`,
/// stays open for `recovery_timeout`, then admits one probe call. Any
/// non-success during a probe re-opens the circuit; a successful probe
/// closes it.
pub struct CircuitBreaker {
    inner: Mutex<Inner>,
}

impl CircuitBreaker {
    #[must_use]
    pub fn new(config: CircuitBreakerConfig) -> Self {
        Self {
            inner: Mutex::new(Inner {
                config,
                state: State::Closed,
                failure_timestamps: Vec::new(),
            }),
        }
    }

    /// Run `f` under the breaker.
    ///
    /// Returns `DomainError::CircuitOpen` without invoking `f` if the circuit
    /// is rejecting calls. Otherwise executes `f` and records the outcome:
    /// infrastructure-shaped errors count as failures, caller-induced errors
    /// are ignored, and during a `HalfOpen` probe any non-success re-opens the
    /// circuit.
    ///
    /// # Errors
    ///
    /// Propagates any `DomainError` returned by `f`, or `DomainError::CircuitOpen`
    /// when the circuit is open.
    pub async fn execute<F, Fut, T>(&self, f: F) -> Result<T, DomainError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, DomainError>>,
    {
        // The lock guard returned by `self.inner.lock()` MUST be dropped before
        // the `.await` below — holding a `parking_lot::Mutex` guard across an
        // await point would block the executor thread on every contended call.
        // The temporary-drop rule on this single-expression `let` is load-bearing:
        // a refactor to `let mut g = self.inner.lock(); let was_probe = g.try_acquire()?; … f().await`
        // would silently violate it. The module-scoped `#[deny(clippy::await_holding_lock)]`
        // above is the static guard against that regression.
        let was_probe = self.inner.lock().try_acquire()?;

        let result = f().await;

        let mut inner = self.inner.lock();
        match &result {
            Ok(_) => inner.record_success(),
            Err(e) if is_health_failure(e) => inner.record_failure(),
            // HalfOpen is strict: any non-success during the probe re-opens.
            Err(_) if was_probe => inner.record_failure(),
            // Caller-induced error in Closed state — leave breaker untouched.
            Err(_) => {}
        }

        result
    }
}

/// Returns `true` when `err` indicates plugin/infrastructure ill-health and
/// must trip the circuit breaker. Caller-induced errors return `false`.
fn is_health_failure(err: &DomainError) -> bool {
    match err {
        DomainError::TypesRegistryUnavailable(_)
        | DomainError::ClientHub(_)
        | DomainError::PluginNotFound { .. }
        | DomainError::PluginUnavailable { .. }
        | DomainError::Timeout
        | DomainError::Internal(_) => true,
        DomainError::Plugin(canonical) => is_canonical_health_failure(canonical),
        // Already-open or invalid-config errors are not new failure signals.
        DomainError::CircuitOpen
        | DomainError::InvalidPluginInstance { .. }
        | DomainError::ModuleNotConfigured { .. } => false,
    }
}

/// Canonical-error variants that the breaker treats as plugin ill-health.
///
/// Kept aligned with [`crate::infra`] / `usage-emitter`'s `DeliveryHandler` retry set so
/// the two layers don't contradict each other: there, `ServiceUnavailable` and
/// `DeadlineExceeded` are retried (recoverable infra failures), while `Internal`,
/// `Unknown`, and `DataLoss` are dead-lettered (permanent — serious defect, unrecognized
/// error space, or unrecoverable corruption). Tripping the breaker on the latter would
/// open the ingest path for problems that cannot be fixed by waiting and would
/// double-classify the same canonical variants as both transient and permanent.
///
/// Genuine transient infra conditions on the local-client side (plugin not ready, types
/// registry down, transport errors) surface through the dedicated `DomainError` variants
/// in [`is_health_failure`] above, not through `Plugin(Internal { .. })`.
fn is_canonical_health_failure(err: &UsageCollectorError) -> bool {
    matches!(
        err,
        UsageCollectorError::ServiceUnavailable { .. }
            | UsageCollectorError::DeadlineExceeded { .. }
    )
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "circuit_breaker_tests.rs"]
mod circuit_breaker_tests;
