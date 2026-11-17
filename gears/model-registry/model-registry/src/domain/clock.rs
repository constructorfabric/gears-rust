//! Injectable clock (DESIGN §4 Testability): one source for audit and
//! approval timestamps and for chain cache expiry.

#[cfg(any(test, feature = "test-support"))]
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use toolkit_macros::domain_model;

pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// The wall clock.
#[domain_model]
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock that moves only when told to; for tests.
#[cfg(any(test, feature = "test-support"))]
#[domain_model]
#[derive(Debug)]
pub struct ManualClock(Mutex<DateTime<Utc>>);

#[cfg(any(test, feature = "test-support"))]
impl ManualClock {
    #[must_use]
    pub fn new(now: DateTime<Utc>) -> Self {
        Self(Mutex::new(now))
    }

    /// Starts at a fixed, recognizable instant.
    #[must_use]
    pub fn fixed() -> Self {
        Self::new(
            DateTime::parse_from_rfc3339("2026-10-26T12:00:00Z")
                .map_or_else(|_| Utc::now(), |t| t.with_timezone(&Utc)),
        )
    }

    pub fn set(&self, now: DateTime<Utc>) {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = now;
    }

    pub fn advance(&self, by: chrono::Duration) {
        let mut guard = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard += by;
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
