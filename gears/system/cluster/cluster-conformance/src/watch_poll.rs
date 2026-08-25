// Created: 2026-08-03 by Constructor Tech
//! One bounded polling strategy, shared by every watch-driven scenario.
//!
//! Every suite in this crate faces the same problem: drive a watch until an
//! event satisfies a predicate, without letting a notification/polling
//! regression hang the whole suite (PGR-D4). [`poll_watch`] is that single
//! strategy — the cache and leader watches both reach it through
//! [`PollableWatch`], so a scenario never hand-rolls a `match watch.recv()`
//! loop with its own bounds.
//!
//! A poll is bounded two ways, both named and both overridable per call site:
//!
//! * [`DEFAULT_MAX_SKIPPED_EVENTS`] caps how many non-matching events it will
//!   skip before giving up — override with [`WatchPoll::max_skipped`].
//! * [`DEFAULT_WAIT_BUDGET`] wraps the whole poll in a timeout, so an event that
//!   never arrives (each individual `recv().await` is otherwise unbounded)
//!   resolves to a verdict rather than blocking forever — override with
//!   [`WatchPoll::upto`].
//!
//! Both bounds are backend-independent. Under
//! [`TimeControl::Virtual`](crate::time::TimeControl) a paused clock
//! auto-advances to the timeout while the runtime is idle, so an already-queued
//! event still returns immediately and an absent one still resolves at once — no
//! wall-clock cost, and the same code path as a real backend's NOTIFY / polling
//! round-trip. The defaults are generous enough for that round-trip; a scenario
//! that needs a tighter or looser bound says so at the call site:
//!
//! ```ignore
//! // Default bounds: 3s / 64 skipped events.
//! let seen = poll_watch(&mut watch).until(|e| matches!(e, CacheEvent::Changed { .. })).await;
//!
//! // Tighter budget: prove a *second* event does not arrive, without paying 3s for it.
//! let duplicate = poll_watch(&mut watch)
//!     .upto(Duration::from_millis(250))
//!     .until(|e| matches!(e, CacheEvent::Changed { .. }))
//!     .await;
//! ```

use std::fmt;
use std::time::Duration;

use cluster_sdk::cache::{CacheEvent, CacheWatch, CacheWatchEvent};
use cluster_sdk::error::ClusterError;
use cluster_sdk::leader::{LeaderStatus, LeaderWatch, LeaderWatchEvent};

/// Overall wait budget a poll uses unless the call site calls
/// [`WatchPoll::upto`]. Generous enough for a real backend's NOTIFY / polling
/// round-trip; free under a paused clock.
pub const DEFAULT_WAIT_BUDGET: Duration = Duration::from_secs(3);

/// How many non-matching events a poll skips unless the call site calls
/// [`WatchPoll::max_skipped`].
pub const DEFAULT_MAX_SKIPPED_EVENTS: usize = 64;

/// How a watch stopped producing events.
pub enum WatchEnd {
    /// The watch delivered its terminal `Closed` event.
    Closed(ClusterError),
    /// The channel dropped without a terminal event (the backend went away).
    Dropped,
}

impl fmt::Display for WatchEnd {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed(err) => write!(f, "watch closed: {err}"),
            Self::Dropped => write!(f, "watch dropped without a Closed event"),
        }
    }
}

/// One event, classified for the poll loop.
pub enum PollStep<T> {
    /// A non-terminal event carrying the watch's payload.
    Payload(T),
    /// A non-terminal signal that carries no payload (`Lagged` / `Reset`).
    Skip,
    /// The watch is finished; no further events will arrive.
    End(WatchEnd),
}

/// Why a poll stopped.
pub enum Polled<T> {
    /// An event satisfied the caller's predicate.
    Matched(T),
    /// The watch ended before a matching event arrived.
    Ended(WatchEnd),
    /// The skip cap was reached without a match.
    SkipCap(usize),
    /// The wait budget elapsed without a match.
    TimedOut(Duration),
}

impl<T> Polled<T> {
    /// `true` if an event satisfied the predicate.
    pub fn is_match(&self) -> bool {
        matches!(self, Self::Matched(_))
    }

    /// The matched payload, panicking with `context` and the reason no event
    /// matched. `context` should name the violated scenario, e.g.
    /// `"SC-LEAD-005: no Status event"`.
    pub fn expect_match(self, context: &str) -> T {
        match self {
            Self::Matched(payload) => payload,
            Self::Ended(end) => panic!("{context}: {end}"),
            Self::SkipCap(cap) => panic!("{context}: skipped {cap} events without a match"),
            Self::TimedOut(budget) => {
                panic!(
                    "{context}: no matching event within {}ms",
                    budget.as_millis()
                )
            }
        }
    }
}

/// A watch this module can poll generically. Implemented for every watch the
/// SDK hands a scenario; the associated payload is what a predicate sees, with
/// the wrapper's `Lagged`/`Reset`/`Closed` variants already classified.
pub trait PollableWatch {
    /// The payload carried by a non-terminal event.
    type Payload;

    /// Awaits the next event and classifies it for the poll loop.
    async fn next_step(&mut self) -> PollStep<Self::Payload>;
}

impl PollableWatch for CacheWatch {
    type Payload = CacheEvent;

    async fn next_step(&mut self) -> PollStep<CacheEvent> {
        match self.recv().await {
            Some(CacheWatchEvent::Event(event)) => PollStep::Payload(event),
            Some(CacheWatchEvent::Closed(err)) => PollStep::End(WatchEnd::Closed(err)),
            None => PollStep::End(WatchEnd::Dropped),
            // `Lagged`/`Reset` carry no payload, and the enum is `non_exhaustive`
            // so a future signal must not fail a scenario that predates it.
            Some(_) => PollStep::Skip,
        }
    }
}

impl PollableWatch for LeaderWatch {
    type Payload = LeaderStatus;

    async fn next_step(&mut self) -> PollStep<LeaderStatus> {
        // `changed()` is infallible, so there is no `Dropped` case to classify.
        match self.changed().await {
            LeaderWatchEvent::Status(status) => PollStep::Payload(status),
            LeaderWatchEvent::Closed(err) => PollStep::End(WatchEnd::Closed(err)),
            // See the `CacheWatch` impl: payload-less and future signals skip.
            _ => PollStep::Skip,
        }
    }
}

/// Starts a bounded poll of `watch` with the default bounds. Chain
/// [`upto`](WatchPoll::upto) / [`max_skipped`](WatchPoll::max_skipped) to
/// override them, then a terminal — [`until`](WatchPoll::until),
/// [`first`](WatchPoll::first) or [`first_match`](WatchPoll::first_match).
pub fn poll_watch<W: PollableWatch>(watch: &mut W) -> WatchPoll<'_, W> {
    WatchPoll {
        watch,
        budget: DEFAULT_WAIT_BUDGET,
        max_skipped: DEFAULT_MAX_SKIPPED_EVENTS,
    }
}

/// A bounded poll in progress; see [`poll_watch`].
pub struct WatchPoll<'w, W> {
    watch: &'w mut W,
    budget: Duration,
    max_skipped: usize,
}

impl<W: PollableWatch> WatchPoll<'_, W> {
    /// Overrides [`DEFAULT_WAIT_BUDGET`] for this poll.
    #[must_use]
    pub fn upto(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }

    /// Overrides [`DEFAULT_MAX_SKIPPED_EVENTS`] for this poll.
    #[must_use]
    pub fn max_skipped(mut self, max_skipped: usize) -> Self {
        self.max_skipped = max_skipped;
        self
    }

    /// Resolves `true` once an event satisfies `pred`, `false` on any other
    /// outcome. Use when the assertion is only about whether the event arrived;
    /// [`first_match`](Self::first_match) when the failure message should say
    /// *why* none did.
    pub async fn until<P>(self, pred: P) -> bool
    where
        P: Fn(&W::Payload) -> bool,
    {
        self.first_match(|payload| pred(&payload).then_some(()))
            .await
            .is_match()
    }

    /// Resolves with the next payload the watch delivers, whatever it is.
    pub async fn first(self) -> Polled<W::Payload> {
        self.first_match(Some).await
    }

    /// Resolves with the first payload `classify` maps to `Some`, so a scenario
    /// can both select an event and extract from it in one poll.
    pub async fn first_match<T, F>(self, classify: F) -> Polled<T>
    where
        F: Fn(W::Payload) -> Option<T>,
    {
        let Self {
            watch,
            budget,
            max_skipped,
        } = self;
        let poll = async {
            for _ in 0..max_skipped {
                match watch.next_step().await {
                    PollStep::Payload(payload) => {
                        if let Some(matched) = classify(payload) {
                            return Polled::Matched(matched);
                        }
                    }
                    PollStep::Skip => {}
                    PollStep::End(end) => return Polled::Ended(end),
                }
            }
            Polled::SkipCap(max_skipped)
        };
        tokio::time::timeout(budget, poll)
            .await
            .unwrap_or(Polled::TimedOut(budget))
    }
}
