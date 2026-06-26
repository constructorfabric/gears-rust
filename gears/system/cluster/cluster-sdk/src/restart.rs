// Created: 2026-06-10 by Constructor Tech
//! The opt-in watch auto-restart combinator shared by all three cluster watches.
//!
//! Long-lived watches over a remote backend close terminally on transient
//! failures (`ConnectionLost`, `Timeout`, `ResourceExhausted`). Without a
//! shipped combinator every consumer reinvents the same reconnect loop with
//! inconsistent backoff and retryability classification, producing
//! thundering-herd reconnect storms against a recovering backend (DESIGN §3.9).
//!
//! [`RestartingWatch`] turns a *retryable* terminal [`Closed`](crate::cache::CacheWatchEvent::Closed)
//! into transparent reconnection with jittered backoff, synthesizes a `Reset`
//! to the consumer on each successful resubscribe (so it re-reads state per
//! ADR-003), and propagates *non-retryable* and `Shutdown` closes unchanged.
//! Retryability is read from [`ClusterError::is_retryable`], which already
//! encodes the DESIGN §3.9 classification table verbatim
//! ([`ProviderErrorKind::ConnectionLost`](crate::ProviderErrorKind::ConnectionLost),
//! `Timeout`, `ResourceExhausted` retryable; everything else not).
//!
//! Wrap a watch via [`CacheWatch::auto_restart`](crate::cache::CacheWatch::auto_restart),
//! [`LeaderWatch::auto_restart`](crate::leader::LeaderWatch::auto_restart), or
//! [`ServiceWatch::auto_restart`](crate::discovery::ServiceWatch::auto_restart).
//! Consumers wanting a custom restart loop keep consuming the raw `*WatchEvent`
//! stream without the combinator — the combinator is strictly opt-in.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use rand::RngExt;

use crate::cache::{CacheWatch, CacheWatchEvent};
use crate::discovery::{ServiceWatch, ServiceWatchEvent};
use crate::error::ClusterError;
use crate::leader::{LeaderStatus, LeaderWatch, LeaderWatchEvent};
use crate::observability::{ClusterMetrics, logs, primitive};

/// A boxed, owned future yielding a freshly resubscribed watch.
pub(crate) type ResubscribeFuture<W> =
    Pin<Box<dyn Future<Output = Result<W, ClusterError>> + Send>>;

/// A boxed future yielding the next event of a wrapped watch (borrowing it for
/// `'a`), or `None` at end of stream.
type NextEventFuture<'a, E> = Pin<Box<dyn Future<Output = Option<E>> + Send + 'a>>;

mod sealed {
    /// Seals [`RestartableWatch`](super::RestartableWatch) so only the three SDK
    /// watch types implement it.
    pub trait Sealed {}
}

/// The behaviour [`RestartingWatch`] needs from a wrapped watch.
///
/// Sealed — implemented only by [`CacheWatch`], [`LeaderWatch`], and
/// [`ServiceWatch`]. It unifies the three watches' differing event accessors
/// (`recv` / `changed`) and exposes the internal resubscribe seam the facade
/// installs (see each `*V1::watch`/`elect`).
pub trait RestartableWatch: sealed::Sealed + Send + Sized + 'static {
    /// The watch's union event type (`CacheWatchEvent`, `LeaderWatchEvent`, or
    /// `ServiceWatchEvent`).
    type Event: Send;

    /// The bounded `primitive` label this watch's reset signals carry
    /// (`cache` / `leader` / `discovery`).
    const PRIMITIVE: &'static str;

    /// The `(provider, metrics)` observability context the backend stamped on the
    /// watch, or `None` when no metrics are wired. Read once by
    /// [`RestartingWatch`] so it can emit `cluster_watch_resets_total` and the
    /// `cluster.watch.reset` log on each transparent reconnect.
    fn observability(&self) -> Option<(&'static str, Arc<dyn ClusterMetrics>)>;

    /// Awaits the next raw event, or `None` once the underlying stream has ended
    /// without a terminal `Closed`.
    fn next_event(&mut self) -> NextEventFuture<'_, Self::Event>;

    /// Borrows the [`ClusterError`] of a terminal `Closed(err)` event, or `None`
    /// for any non-terminal variant.
    fn closed_error(event: &Self::Event) -> Option<&ClusterError>;

    /// The synthesized `Reset` event emitted to the consumer on each successful
    /// resubscribe.
    fn reset_event() -> Self::Event;

    /// Whether this watch carries the facade-installed resubscribe seam. A watch
    /// built from a bare `channel(..)` (a backend or test stub) has none, so a
    /// retryable close is propagated unchanged rather than reconnected.
    fn can_resubscribe(&self) -> bool;

    /// Resubscribes via the installed seam, producing a fresh watch. Only called
    /// when [`can_resubscribe`](Self::can_resubscribe) is `true`.
    fn resubscribe(&self) -> ResubscribeFuture<Self>;
}

/// The retry policy driving [`RestartingWatch`] reconnection (DESIGN §3.1, §3.9).
///
/// [`default`](Self::default) is exponential backoff `1s → 30s` with full jitter
/// (`jitter_factor: 1.0`) and no retry cap (`max_retries: None`) — the
/// recommended default for a recovering backend. Override by constructing the
/// struct directly, for example
/// `RetryPolicy { max_retries: Some(5), ..RetryPolicy::default() }`.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// The first reconnect delay; each subsequent attempt doubles it up to
    /// [`max_backoff`](Self::max_backoff).
    pub initial_backoff: Duration,
    /// The ceiling the exponential growth is clamped to.
    pub max_backoff: Duration,
    /// Full-jitter fraction in `0.0..=1.0`. `1.0` spreads each delay uniformly
    /// over `(0, computed]`; `0.0` disables jitter (exact exponential delays).
    pub jitter_factor: f32,
    /// Optional cap on consecutive reconnect attempts. `None` retries forever;
    /// `Some(n)` propagates the most recent `Closed(err)` once `n` attempts are
    /// exhausted.
    pub max_retries: Option<u32>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            jitter_factor: 1.0,
            max_retries: None,
        }
    }
}

impl RetryPolicy {
    /// The delay before the reconnect attempt numbered `attempt` (0-based):
    /// `min(initial_backoff · 2^attempt, max_backoff)` with full jitter applied.
    ///
    /// Both the exponential growth and the `2^attempt` factor saturate, so a
    /// large `attempt` clamps to `max_backoff` rather than overflowing.
    fn backoff_for(&self, attempt: u32) -> Duration {
        let base = u32::try_from(2u64.saturating_pow(attempt))
            .ok()
            .and_then(|factor| self.initial_backoff.checked_mul(factor))
            .map_or(self.max_backoff, |grown| grown.min(self.max_backoff));

        let jitter = self.jitter_factor.clamp(0.0, 1.0);
        if jitter == 0.0 {
            return base;
        }
        // Full jitter: scale the computed delay by `1 - jitter·u`, `u ∈ [0, 1)`,
        // spreading reconnects so a fleet recovering together does not stampede.
        let u: f32 = rand::rng().random();
        base.mul_f32(1.0 - jitter * u)
    }
}

/// An opt-in wrapper that transparently reconnects a wrapped `*Watch` on
/// retryable terminal closes (DESIGN §3.9). Construct via `*Watch::auto_restart`.
///
/// Consume it exactly like the raw watch via [`recv`](Self::recv): retryable
/// closes are absorbed internally (backoff, resubscribe, synthesized `Reset`),
/// while non-retryable and `Shutdown` closes are returned unchanged as the
/// terminal event — after which `recv` yields `None`.
#[must_use = "a RestartingWatch yields no events unless polled via `recv`"]
pub struct RestartingWatch<W: RestartableWatch> {
    inner: W,
    policy: RetryPolicy,
    /// Consecutive reconnect attempts since the last success; resets to 0 on a
    /// successful resubscribe and bounds against `policy.max_retries`.
    attempts: u32,
    /// Set once a non-retryable close (or an exhausted cap) has been returned;
    /// thereafter `recv` yields `None`.
    terminated: bool,
    /// The `(provider, metrics)` context captured from the wrapped watch at
    /// construction, used to emit the watch-reset signals on each reconnect.
    /// `None` when no metrics were wired (the signals are then skipped).
    observability: Option<(&'static str, Arc<dyn ClusterMetrics>)>,
}

impl<W: RestartableWatch> RestartingWatch<W> {
    /// Wraps `inner` with `policy`. Crate-internal: consumers construct via
    /// `*Watch::auto_restart`.
    pub(crate) fn new(inner: W, policy: RetryPolicy) -> Self {
        // Capture the observability context once: the wrapped watch was stamped by
        // the (instrumented) backend that produced it.
        let observability = inner.observability();
        Self {
            inner,
            policy,
            attempts: 0,
            terminated: false,
            observability,
        }
    }

    /// Emits the watch-reset signals on a successful reconnect:
    /// `cluster_watch_resets_total` and the `cluster.watch.reset` WARN log,
    /// labelled by `provider` and this watch's `primitive`. A no-op when no
    /// metrics context was wired.
    fn emit_reset(&self) {
        if let Some((provider, metrics)) = &self.observability {
            metrics.watch_reset(W::PRIMITIVE);
            tracing::warn!(
                name: logs::WATCH_RESET,
                provider = %provider,
                primitive = W::PRIMITIVE,
                "cluster watch resubscribed after a retryable close"
            );
        }
    }

    /// Awaits the next event, transparently reconnecting on retryable closes.
    ///
    /// Returns:
    /// - `Some(event)` for ordinary events plus `Lagged`/`Reset` — passed through
    ///   unchanged;
    /// - `Some(reset_event)` after a successful reconnect (the synthesized
    ///   `Reset` telling the consumer to re-read state) — whether the reconnect
    ///   was triggered by a retryable `Closed` or by the stream ending in `None`
    ///   on a resubscribable watch;
    /// - `Some(Closed(err))` once, for a non-retryable close, an exhausted retry
    ///   cap, or a retryable close on a seam-less watch — then the watch is
    ///   terminal;
    /// - `None` once terminal, or if the underlying stream ended without a
    ///   `Closed` on a watch with no resubscribe seam (or after the retry cap is
    ///   exhausted reconnecting one).
    pub async fn recv(&mut self) -> Option<W::Event> {
        if self.terminated {
            return None;
        }
        let Some(event) = self.inner.next_event().await else {
            // The underlying stream ended without a terminal Closed. On a watch
            // with a resubscribe seam this is a transient sender-drop — a remote
            // backend's connection task dropping the sender on a transient fault,
            // the canonical reconnect trigger — so treat it like a retryable
            // close: reconnect with backoff and synthesize a Reset so the consumer
            // re-reads state (ADR-003). A seam-less watch (a bare channel / test
            // stub) genuinely ended, and an exhausted retry cap is terminal.
            if self.inner.can_resubscribe() && self.reconnect().await {
                return Some(W::reset_event());
            }
            self.terminated = true;
            return None;
        };
        match W::closed_error(&event) {
            // Not a terminal close: ordinary event, Lagged, or Reset.
            None => Some(event),
            Some(err) => {
                if !err.is_retryable() || !self.inner.can_resubscribe() {
                    // Non-retryable, or retryable but no resubscribe seam —
                    // propagate the close unchanged and stop.
                    self.terminated = true;
                    return Some(event);
                }
                // Retryable and reconnectable: absorb the close and reconnect.
                if self.reconnect().await {
                    Some(W::reset_event())
                } else {
                    // Retry cap exhausted — propagate the most recent close.
                    self.terminated = true;
                    Some(event)
                }
            }
        }
    }

    /// Reconnects with jittered backoff, honoring `policy.max_retries`. Returns
    /// `true` once resubscribed (and resets `attempts`), or `false` when the cap
    /// is exhausted. A failed resubscribe attempt counts toward the cap and is
    /// retried with grown backoff.
    async fn reconnect(&mut self) -> bool {
        loop {
            if self
                .policy
                .max_retries
                .is_some_and(|cap| self.attempts >= cap)
            {
                return false;
            }
            let delay = self.policy.backoff_for(self.attempts);
            self.attempts = self.attempts.saturating_add(1);
            tokio::time::sleep(delay).await;
            match self.inner.resubscribe().await {
                Ok(fresh) => {
                    self.inner = fresh;
                    self.attempts = 0;
                    self.emit_reset();
                    return true;
                }
                // A non-retryable resubscribe failure (auth, shutdown, unsupported)
                // will never succeed; stop now rather than spin until the cap — or
                // forever when `max_retries` is `None`.
                Err(err) if !err.is_retryable() => return false,
                // Retryable: fall through to retry with grown backoff toward the cap.
                Err(_) => {}
            }
        }
    }
}

/// Leadership-specific accessors. A [`RestartingWatch<LeaderWatch>`] still
/// exposes the synchronous gate-pattern reads and explicit step-down of the
/// wrapped [`LeaderWatch`]; reconnection swaps the inner watch, so each reads the
/// current subscription's state.
impl RestartingWatch<LeaderWatch> {
    /// The current cached leadership snapshot — see [`LeaderWatch::status`].
    #[must_use]
    pub fn status(&self) -> LeaderStatus {
        self.inner.status()
    }

    /// `true` when the cached snapshot is [`LeaderStatus::Leader`] — see
    /// [`LeaderWatch::is_leader`] for the advisory staleness bound.
    #[must_use]
    pub fn is_leader(&self) -> bool {
        self.inner.is_leader()
    }

    /// Explicitly resigns the current claim — see [`LeaderWatch::resign`].
    /// Consumes the combinator.
    ///
    /// # Errors
    /// Propagates the backend's resign outcome (DESIGN §3.7).
    pub async fn resign(self) -> Result<(), ClusterError> {
        self.inner.resign().await
    }
}

// --- RestartableWatch impls (sealed to the three SDK watch types) ------------

impl sealed::Sealed for CacheWatch {}
impl RestartableWatch for CacheWatch {
    type Event = CacheWatchEvent;

    const PRIMITIVE: &'static str = primitive::CACHE;

    fn observability(&self) -> Option<(&'static str, Arc<dyn ClusterMetrics>)> {
        self.observability_context()
    }

    fn next_event(&mut self) -> NextEventFuture<'_, CacheWatchEvent> {
        Box::pin(self.recv())
    }

    fn closed_error(event: &CacheWatchEvent) -> Option<&ClusterError> {
        match event {
            CacheWatchEvent::Closed(err) => Some(err),
            _ => None,
        }
    }

    fn reset_event() -> CacheWatchEvent {
        CacheWatchEvent::Reset
    }

    fn can_resubscribe(&self) -> bool {
        self.can_resubscribe()
    }

    fn resubscribe(&self) -> ResubscribeFuture<Self> {
        self.try_resubscribe()
            .unwrap_or_else(|| Box::pin(async { Err(ClusterError::Shutdown) }))
    }
}

impl sealed::Sealed for ServiceWatch {}
impl RestartableWatch for ServiceWatch {
    type Event = ServiceWatchEvent;

    const PRIMITIVE: &'static str = primitive::DISCOVERY;

    fn observability(&self) -> Option<(&'static str, Arc<dyn ClusterMetrics>)> {
        self.observability_context()
    }

    fn next_event(&mut self) -> NextEventFuture<'_, ServiceWatchEvent> {
        Box::pin(self.recv())
    }

    fn closed_error(event: &ServiceWatchEvent) -> Option<&ClusterError> {
        match event {
            ServiceWatchEvent::Closed(err) => Some(err),
            _ => None,
        }
    }

    fn reset_event() -> ServiceWatchEvent {
        ServiceWatchEvent::Reset
    }

    fn can_resubscribe(&self) -> bool {
        self.can_resubscribe()
    }

    fn resubscribe(&self) -> ResubscribeFuture<Self> {
        self.try_resubscribe()
            .unwrap_or_else(|| Box::pin(async { Err(ClusterError::Shutdown) }))
    }
}

impl sealed::Sealed for LeaderWatch {}
impl RestartableWatch for LeaderWatch {
    type Event = LeaderWatchEvent;

    const PRIMITIVE: &'static str = primitive::LEADER;

    fn observability(&self) -> Option<(&'static str, Arc<dyn ClusterMetrics>)> {
        self.observability_context()
    }

    fn next_event(&mut self) -> NextEventFuture<'_, LeaderWatchEvent> {
        // `LeaderWatch::changed` is infallible — end-of-stream surfaces as
        // `Closed(Shutdown)`, so it is wrapped in `Some` and never yields `None`.
        Box::pin(async move { Some(self.changed().await) })
    }

    fn closed_error(event: &LeaderWatchEvent) -> Option<&ClusterError> {
        match event {
            LeaderWatchEvent::Closed(err) => Some(err),
            _ => None,
        }
    }

    fn reset_event() -> LeaderWatchEvent {
        LeaderWatchEvent::Reset
    }

    fn can_resubscribe(&self) -> bool {
        self.can_resubscribe()
    }

    fn resubscribe(&self) -> ResubscribeFuture<Self> {
        self.try_resubscribe()
            .unwrap_or_else(|| Box::pin(async { Err(ClusterError::Shutdown) }))
    }
}

#[cfg(test)]
#[path = "restart_tests.rs"]
mod restart_tests;
