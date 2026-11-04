//! Idle deadlines that cost nothing while there is progress.
//!
//! An [`IdleTimer`] records progress as a timestamp only. Its timer (a
//! coarse [`Timers`] expiry) is armed the first time the owner waits and
//! then kept: when it fires it compares the deadline with the latest
//! progress and, if there was some, is re-armed once. Steady traffic
//! therefore touches the timer at most once per `limit`, not once per frame
//! or write.

use std::task::{Context, Poll};
use std::time::Duration;

use tokio::time::Instant;

use crate::timer::{Expiry, Timers};

/// Fires once `limit` has passed without progress.
pub struct IdleTimer {
    limit: Duration,
    last: Instant,
    timers: Timers,
    armed: Option<Expiry>,
}

impl IdleTimer {
    /// Fires `limit` after the latest progress (initially: now).
    pub fn new(limit: Duration, timers: &Timers) -> Self {
        Self {
            limit,
            last: Instant::now(),
            timers: timers.clone(),
            armed: None,
        }
    }

    /// The idle budget.
    pub fn limit(&self) -> Duration {
        self.limit
    }

    /// Record progress: the deadline moves to `limit` from now. Touches no
    /// timer.
    pub fn touch(&mut self) {
        self.last = Instant::now();
    }

    /// Ready once `limit` has passed since the latest progress. Call it
    /// while waiting; it registers the waker on the timer. A `limit` too
    /// large to represent as a deadline never expires.
    pub fn poll_expired(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        loop {
            let Some(due) = self.last.checked_add(self.limit) else {
                return Poll::Pending;
            };
            let timers = &self.timers;
            let expiry = self.armed.get_or_insert_with(|| timers.expiry(due));
            if expiry.poll_elapsed(cx).is_pending() {
                return Poll::Pending;
            }
            // The timer may have been armed for an earlier deadline: only
            // the latest progress counts.
            if Instant::now() >= due {
                return Poll::Ready(());
            }
            self.armed = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Let the timer driver run after the clock moved, then poll once.
    async fn expired(t: &mut IdleTimer) -> bool {
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        let mut cx = Context::from_waker(std::task::Waker::noop());
        t.poll_expired(&mut cx).is_ready()
    }

    #[tokio::test(start_paused = true)]
    async fn progress_pushes_the_deadline_back() {
        let mut t = IdleTimer::new(Duration::from_secs(10), &Timers::new());
        assert!(!expired(&mut t).await);
        tokio::time::advance(Duration::from_secs(8)).await;
        t.touch();
        tokio::time::advance(Duration::from_secs(8)).await;
        assert!(!expired(&mut t).await, "8 s since progress");
        tokio::time::advance(Duration::from_secs(2) + crate::timer::TICK).await;
        assert!(
            expired(&mut t).await,
            "10 s since progress (plus at most a tick)"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn silence_fires_after_the_limit() {
        let mut t = IdleTimer::new(Duration::from_millis(300), &Timers::new());
        assert!(!expired(&mut t).await);
        tokio::time::advance(Duration::from_millis(299)).await;
        assert!(!expired(&mut t).await);
        tokio::time::advance(Duration::from_millis(1) + crate::timer::TICK).await;
        assert!(expired(&mut t).await, "fires at most a tick late");
    }

    #[tokio::test(start_paused = true)]
    async fn an_unrepresentable_limit_never_expires() {
        let mut t = IdleTimer::new(Duration::MAX, &Timers::new());
        assert!(!expired(&mut t).await);
        tokio::time::advance(Duration::from_secs(3600)).await;
        assert!(!expired(&mut t).await);
    }

    #[tokio::test(start_paused = true)]
    async fn waiting_wakes_the_task_when_it_expires() {
        let mut t = IdleTimer::new(Duration::from_millis(50), &Timers::new());
        let started = Instant::now();
        std::future::poll_fn(|cx| t.poll_expired(cx)).await;
        let waited = started.elapsed();
        assert!(waited >= Duration::from_millis(50), "{waited:?}");
        assert!(
            waited <= Duration::from_millis(50) + crate::timer::TICK,
            "{waited:?}"
        );
    }
}
