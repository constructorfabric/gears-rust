// Created: 2026-07-15 by Constructor Tech
//! [`TimeControl`] — how a time-sensitive conformance scenario makes time pass.
//!
//! Time-dependent scenarios (TTL expiry, lock timeout, leader renewal) need to
//! fast-forward past an interval without a test waiting real seconds. The
//! original suites did this with [`tokio::time::pause`] + [`tokio::time::advance`]
//! (virtual time), which is instant and deterministic — but only correct for
//! **in-memory** fixture backends. Against a backend over a real `sqlx` pool /
//! network, a paused virtual clock spuriously fires `sqlx`'s own internal
//! pool-acquire timeout: the paused runtime auto-advances the clock to the next
//! pending timer deadline while real network I/O is parked, so the acquire
//! `tokio::time::timeout` wins the race and every `pool.acquire()` returns a
//! bogus `PoolTimedOut` even on a completely free pool (see the postgres cluster
//! plugin's `docs/GAP-SOLUTIONS.md` §3 for the full trace).
//!
//! [`TimeControl`] lets one scenario body serve both worlds: fixture-backed
//! callers pass [`TimeControl::Virtual`] (unchanged fast/deterministic
//! behavior); real-backend callers pass [`TimeControl::Real`], which swaps the
//! virtual advance for a real (bounded) `tokio::time::sleep` and never pauses the
//! clock, so `sqlx`'s timers behave normally.

use std::time::Duration;

/// Selects the clock model a time-sensitive scenario uses.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TimeControl {
    /// Freeze the clock ([`tokio::time::pause`]) and fast-forward deterministically
    /// ([`tokio::time::advance`]). Instant and deterministic, but **only** correct
    /// for in-memory fixture backends that never do real I/O — a paused clock
    /// breaks a real `sqlx` pool's acquire timeout (see module docs).
    Virtual,
    /// Let real wall-clock time pass ([`tokio::time::sleep`]); never pause the
    /// clock. Required for any backend over a real connection pool / network.
    /// Callers running reaper-driven backends (e.g. Postgres TTL reclaim) must
    /// configure a reaper/sweep interval comfortably shorter than the scenario's
    /// [`elapse`](TimeControl::elapse) durations so the background reclaim
    /// actually fires within the real wait.
    Real,
}

impl TimeControl {
    /// Upper bound on a single real [`elapse`](Self::elapse) sleep. A scenario
    /// may advance virtual time by an implausibly large amount (e.g. an hour, to
    /// prove a TTL-less entry is never swept); under [`Real`](Self::Real) that
    /// must not translate into an hour-long test, so the real sleep is capped —
    /// a few sweep intervals is enough to prove the same property.
    const REAL_CAP: Duration = Duration::from_millis(500);

    /// Freezes virtual time under [`Virtual`](Self::Virtual); a no-op under
    /// [`Real`](Self::Real). Call once at the start of a time-sensitive scenario.
    pub fn begin(self) {
        if self == Self::Virtual {
            tokio::time::pause();
        }
    }

    /// Resumes virtual time under [`Virtual`](Self::Virtual); a no-op under
    /// [`Real`](Self::Real). Call once at the end of a time-sensitive scenario.
    pub fn end(self) {
        if self == Self::Virtual {
            tokio::time::resume();
        }
    }

    /// Advances scenario time, then yields so background reaper / renewal /
    /// sweeper tasks get a chance to run. Under [`Virtual`](Self::Virtual) this
    /// advances the clock by exactly `dur` (instant [`tokio::time::advance`]);
    /// under [`Real`](Self::Real) it sleeps for `dur.min(REAL_CAP)` — i.e. **at
    /// most** [`REAL_CAP`](Self::REAL_CAP), *not* the full `dur` — so a scenario
    /// that advances an implausibly large virtual interval does not become a
    /// multi-second real test. Timing assertions that require the full `dur` to
    /// have elapsed are therefore only valid under `Virtual`.
    pub async fn elapse(self, dur: Duration) {
        match self {
            Self::Virtual => tokio::time::advance(dur).await,
            Self::Real => tokio::time::sleep(dur.min(Self::REAL_CAP)).await,
        }
        tokio::task::yield_now().await;
    }
}
