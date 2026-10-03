//! Monotonic clock port for the secret write's lease guard.
//!
//! The guard compares a process-monotonic elapsed time against the intent
//! lease, so it must never read wall-clock time (which can jump). The port
//! exists so tests can move time without sleeping.

use std::time::Instant;

use toolkit_macros::domain_model;

/// Source of process-monotonic instants.
pub trait MonotonicClock: Send + Sync + 'static {
    fn now(&self) -> Instant;
}

/// The real clock: [`Instant::now`].
#[domain_model]
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl MonotonicClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}
