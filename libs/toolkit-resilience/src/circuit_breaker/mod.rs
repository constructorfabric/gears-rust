//! Circuit breaker for calls to one backend.
//!
//! A [`CircuitBreaker`] guards one client-to-backend pair. Every call asks it
//! for a [`Permit`] first and records the outcome on the permit afterwards:
//!
//! - **Closed**: calls go through and outcomes are counted over a sliding time
//!   window. When at least `minimum_calls` were recorded and the failure share
//!   reaches `failure_ratio` (or `k` failures came in a row, with
//!   [`CircuitBreakerConfig::with_consecutive_failures`]), the breaker opens.
//! - **Open**: calls are rejected at once with [`Rejected`], which carries the
//!   time left until the breaker lets probes through.
//! - **Half-open**: after `open_for`, up to `half_open_calls` probes go
//!   through. One failure opens the breaker again; when every probe has
//!   succeeded it closes and the window starts empty.
//!
//! What counts as a failure is the caller's decision ([`Outcome`]). A permit
//! dropped without an outcome (the call was cancelled, or failed before it
//! reached the backend) counts as nothing and returns its half-open slot.
//!
//! Outcomes recorded after the breaker changed state are ignored: a call that
//! started before the breaker opened says nothing about the backend now.
//!
//! The design follows the state machine of `tower-resilience-circuitbreaker`
//! (MIT OR Apache-2.0): a lock-free closed-state check through an atomic
//! mirror of the state, and a fresh semaphore per half-open cycle so a late
//! probe from an earlier cycle cannot take a slot of the current one. The
//! window is time-based with fixed buckets, so memory does not grow with the
//! call rate.

mod config;
#[cfg(feature = "tower")]
mod layer;
mod window;

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

pub use config::{CircuitBreakerConfig, ConfigError};
#[cfg(feature = "tower")]
pub use layer::{
    BoxError, Bypass, CircuitBreakerLayer, CircuitBreakerService, Classify, ErrorsAreFailures,
    ResponseFuture,
};
use window::Window;

/// How long a caller rejected because every half-open slot is taken should
/// wait. A slot frees up as soon as a probe finishes, so this is short and
/// not tied to `open_for`.
const HALF_OPEN_BUSY_RETRY: Duration = Duration::from_secs(1);

/// State of a [`CircuitBreaker`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Calls go through and outcomes are counted.
    Closed,
    /// Calls are rejected.
    Open,
    /// A limited number of probes go through.
    HalfOpen,
}

impl State {
    const fn as_u8(self) -> u8 {
        match self {
            State::Closed => 0,
            State::Open => 1,
            State::HalfOpen => 2,
        }
    }

    const fn from_u8(v: u8) -> Self {
        match v {
            1 => State::Open,
            2 => State::HalfOpen,
            _ => State::Closed,
        }
    }
}

/// What a finished call says about the backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The backend answered.
    Success,
    /// The backend did not answer usefully: unreachable, timed out, or
    /// reported itself unavailable.
    Failure,
    /// The call says nothing about the backend (a local error, a cancelled
    /// call). Not counted.
    Ignore,
}

/// A call refused because the breaker is open.
#[derive(Debug, Clone, thiserror::Error)]
pub struct Rejected {
    retry_after: Duration,
    name: Option<Arc<str>>,
}

impl Rejected {
    /// How long until the breaker lets a probe through.
    #[must_use]
    pub fn retry_after(&self) -> Duration {
        self.retry_after
    }

    /// The name of the breaker that refused the call, if it has one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

impl fmt::Display for Rejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Some(name) => write!(f, "circuit breaker `{name}` is open")?,
            None => f.write_str("circuit breaker is open")?,
        }
        write!(f, "; retry after {} ms", self.retry_after.as_millis())
    }
}

/// A circuit breaker for one client-to-backend pair. See the
/// [module docs](self).
pub struct CircuitBreaker {
    name: Option<Arc<str>>,
    config: CircuitBreakerConfig,
    state: AtomicU8,
    generation: AtomicU64,
    inner: Mutex<Inner>,
}

struct Inner {
    state: State,
    generation: u64,
    changed_at: Instant,
    window: Window,
    consecutive_failures: u32,
    half_open: Arc<Semaphore>,
    half_open_successes: u32,
}

impl fmt::Debug for CircuitBreaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CircuitBreaker")
            .field("name", &self.name)
            .field("config", &self.config)
            .field("state", &self.state())
            .finish_non_exhaustive()
    }
}

impl CircuitBreaker {
    /// Build a closed breaker.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when the config fails
    /// [`CircuitBreakerConfig::validate`].
    pub fn new(config: CircuitBreakerConfig) -> Result<Self, ConfigError> {
        config.validate()?;
        Ok(Self::closed(config))
    }

    /// A breaker that lets every call through and records nothing.
    #[must_use]
    pub fn disabled() -> Self {
        Self::closed(CircuitBreakerConfig::disabled())
    }

    fn closed(config: CircuitBreakerConfig) -> Self {
        let now = Instant::now();
        Self {
            name: None,
            inner: Mutex::new(Inner {
                state: State::Closed,
                generation: 0,
                changed_at: now,
                window: Window::new(config.window(), now),
                consecutive_failures: 0,
                half_open: Arc::new(Semaphore::new(0)),
                half_open_successes: 0,
            }),
            config,
            state: AtomicU8::new(State::Closed.as_u8()),
            generation: AtomicU64::new(0),
        }
    }

    /// Name the breaker in logs and in [`Rejected`].
    #[must_use]
    pub fn with_name(mut self, name: impl Into<Arc<str>>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// The config the breaker was built with.
    #[must_use]
    pub fn config(&self) -> &CircuitBreakerConfig {
        &self.config
    }

    /// The current state. An open breaker whose `open_for` has passed still
    /// reports `Open` until the next call moves it to half-open.
    #[must_use]
    pub fn state(&self) -> State {
        State::from_u8(self.state.load(Ordering::Acquire))
    }

    /// Ask to make a call.
    ///
    /// # Errors
    ///
    /// Returns [`Rejected`] while the breaker is open, or while it is
    /// half-open and every probe slot is taken.
    pub fn try_acquire(self: &Arc<Self>) -> Result<Permit, Rejected> {
        if !self.config.enabled() {
            return Ok(Permit::noop());
        }
        // Closed is the common case: no lock.
        if self.state() == State::Closed {
            return Ok(Permit {
                breaker: Some(Arc::clone(self)),
                generation: self.generation.load(Ordering::Acquire),
                slot: None,
            });
        }
        let now = Instant::now();
        let mut inner = self.inner.lock();
        match inner.state {
            State::Closed => Ok(self.permit(inner.generation, None)),
            State::Open => {
                let elapsed = now.saturating_duration_since(inner.changed_at);
                if elapsed >= self.config.open_for() {
                    self.transition(&mut inner, State::HalfOpen, now);
                    self.acquire_probe(&inner)
                } else {
                    Err(self.rejected(self.config.open_for().saturating_sub(elapsed)))
                }
            }
            State::HalfOpen => self.acquire_probe(&inner),
        }
    }

    fn acquire_probe(self: &Arc<Self>, inner: &Inner) -> Result<Permit, Rejected> {
        match Arc::clone(&inner.half_open).try_acquire_owned() {
            Ok(slot) => Ok(self.permit(inner.generation, Some(slot))),
            Err(_) => Err(self.rejected(HALF_OPEN_BUSY_RETRY.min(self.config.open_for()))),
        }
    }

    fn permit(self: &Arc<Self>, generation: u64, slot: Option<OwnedSemaphorePermit>) -> Permit {
        Permit {
            breaker: Some(Arc::clone(self)),
            generation,
            slot,
        }
    }

    fn rejected(&self, retry_after: Duration) -> Rejected {
        Rejected {
            retry_after,
            name: self.name.clone(),
        }
    }

    fn record(&self, generation: u64, slot: Option<OwnedSemaphorePermit>, outcome: Outcome) {
        if outcome == Outcome::Ignore {
            return;
        }
        let now = Instant::now();
        let mut inner = self.inner.lock();
        if inner.generation != generation {
            return;
        }
        let failure = outcome == Outcome::Failure;
        match inner.state {
            State::Closed => {
                inner.window.record(now, failure);
                inner.consecutive_failures = if failure {
                    inner.consecutive_failures.saturating_add(1)
                } else {
                    0
                };
                if self.should_open(&inner, now) {
                    self.transition(&mut inner, State::Open, now);
                }
            }
            State::HalfOpen if failure => self.transition(&mut inner, State::Open, now),
            State::HalfOpen => {
                // Keep the slot taken: each cycle lets exactly
                // `half_open_calls` probes through.
                if let Some(slot) = slot {
                    slot.forget();
                }
                inner.half_open_successes = inner.half_open_successes.saturating_add(1);
                if inner.half_open_successes >= self.config.half_open_calls() {
                    self.transition(&mut inner, State::Closed, now);
                }
            }
            // No permit is issued while open, and every transition bumps the
            // generation, so a result can never arrive here.
            State::Open => {}
        }
    }

    fn should_open(&self, inner: &Inner, now: Instant) -> bool {
        if let Some(k) = self.config.consecutive_failures() {
            return inner.consecutive_failures >= k;
        }
        let (calls, failures) = inner.window.totals(now);
        calls >= self.config.minimum_calls()
            && f64::from(failures) >= self.config.failure_ratio() * f64::from(calls)
    }

    fn transition(&self, inner: &mut Inner, to: State, now: Instant) {
        inner.state = to;
        inner.generation = inner.generation.wrapping_add(1);
        inner.changed_at = now;
        inner.consecutive_failures = 0;
        match to {
            State::Closed => inner.window.clear(),
            State::HalfOpen => {
                inner.half_open = Arc::new(Semaphore::new(self.half_open_slots()));
                inner.half_open_successes = 0;
            }
            State::Open => {}
        }
        self.generation.store(inner.generation, Ordering::Release);
        self.state.store(to.as_u8(), Ordering::Release);
        let name = self.name.as_deref().unwrap_or("");
        match to {
            State::Open => tracing::warn!(
                breaker = name,
                open_for = ?self.config.open_for(),
                "circuit breaker opened"
            ),
            State::HalfOpen => tracing::debug!(breaker = name, "circuit breaker half-open"),
            State::Closed => tracing::info!(breaker = name, "circuit breaker closed"),
        }
    }

    fn half_open_slots(&self) -> usize {
        usize::try_from(self.config.half_open_calls()).unwrap_or(usize::MAX)
    }
}

/// Leave to make one call. Record what the call said with
/// [`record`](Self::record); dropping the permit records nothing.
#[must_use = "record the call's outcome on the permit"]
pub struct Permit {
    breaker: Option<Arc<CircuitBreaker>>,
    generation: u64,
    slot: Option<OwnedSemaphorePermit>,
}

impl Permit {
    fn noop() -> Self {
        Self {
            breaker: None,
            generation: 0,
            slot: None,
        }
    }

    /// Record the call's outcome.
    pub fn record(mut self, outcome: Outcome) {
        if let Some(breaker) = self.breaker.take() {
            breaker.record(self.generation, self.slot.take(), outcome);
        }
    }

    /// Record a success.
    pub fn success(self) {
        self.record(Outcome::Success);
    }

    /// Record a failure.
    pub fn failure(self) {
        self.record(Outcome::Failure);
    }
}

impl fmt::Debug for Permit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Permit")
            .field("generation", &self.generation)
            .field("probe", &self.slot.is_some())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "tests.rs"]
mod tests;
