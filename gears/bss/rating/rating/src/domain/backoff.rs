//! Growing, jittered pauses between failed attempts (DESIGN §4.4): from one second, doubling up to
//! a cap, each scaled by a random factor in `[0.5, 1]` so attempts that failed together (every
//! reader of an instance when the database or the PDP blips) do not retry together.

use std::time::Duration;

use toolkit_macros::domain_model;

/// The first pause after a failure.
pub const FIRST_PAUSE: Duration = Duration::from_secs(1);

/// The pauses of one retry loop.
#[domain_model]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    first: Duration,
    cap: Duration,
    next: Duration,
}

impl Backoff {
    /// Pauses from [`FIRST_PAUSE`] (or `cap`, if shorter) doubling up to `cap`.
    #[must_use]
    pub fn new(cap: Duration) -> Self {
        let first = FIRST_PAUSE.min(cap);
        Self {
            first,
            cap,
            next: first,
        }
    }

    /// The pause before the next attempt, jittered; the one after it doubles up to the cap.
    pub fn pause(&mut self) -> Duration {
        let pause = jittered(self.next);
        self.next = self.next.saturating_mul(2).min(self.cap);
        pause
    }

    /// The pause [`Self::pause`] jitters next, before the jitter.
    #[must_use]
    pub const fn upcoming(&self) -> Duration {
        self.next
    }

    /// Starts over from the first pause, after an attempt that succeeded.
    pub const fn reset(&mut self) {
        self.next = self.first;
    }
}

/// `backoff` scaled by a random factor in `[0.5, 1]`.
#[must_use]
pub fn jittered(backoff: Duration) -> Duration {
    let mut bytes = [0_u8; 4];
    let unit = if aws_lc_rs::rand::fill(&mut bytes).is_ok() {
        f64::from(u32::from_le_bytes(bytes)) / f64::from(u32::MAX)
    } else {
        1.0
    };
    backoff.mul_f64(0.5 + 0.5 * unit)
}

#[cfg(test)]
#[path = "backoff_tests.rs"]
mod tests;
