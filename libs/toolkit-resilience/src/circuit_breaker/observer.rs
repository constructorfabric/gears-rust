//! Hooks for watching a [`CircuitBreaker`](super::CircuitBreaker): metrics,
//! logs, or a client that reacts to its breaker opening.

use std::sync::Arc;
use std::time::Duration;

use super::{Outcome, State};

/// Receives a breaker's events. Every method does nothing by default, so an
/// observer implements only what it needs and a new event is not a breaking
/// change.
///
/// Events are delivered after the breaker has released its lock, on the task
/// that made the call. An observer may call back into the breaker, but it
/// should be quick, since it runs on the caller's path, and it must not panic:
/// a panic reaches the caller and the rest of that call's events are lost.
///
/// Events from concurrent calls can arrive in a different order than the
/// breaker went through them. Two ways to stay correct:
///
/// - Apply them as commutative updates: [`on_attach`](Self::on_attach) adds
///   the initial state, each [`StateChange`] moves one unit from `from` to
///   `to`, [`on_detach`](Self::on_detach) removes the last state. The totals
///   are right whatever the order.
/// - Or keep the latest [`StateChange::generation`] and drop older events.
///   Generations belong to one breaker and start over in a new one, so keep
///   them per breaker, from its `on_attach` to its `on_detach`, not per name:
///   a breaker rebuilt under the same name would look stale.
pub trait CircuitBreakerObserver: Send + Sync + 'static {
    /// The observer was added to a breaker, or the breaker it observes was
    /// renamed (after an [`on_detach`](Self::on_detach) under the old name).
    fn on_attach(&self, _event: &Lifecycle<'_>) {}

    /// The breaker is dropped, the observer removed, or the breaker renamed.
    fn on_detach(&self, _event: &Lifecycle<'_>) {}

    /// The breaker changed state.
    fn on_state_change(&self, _event: &StateChange<'_>) {}

    /// A call recorded its outcome.
    fn on_call(&self, _event: &CallRecorded<'_>) {}

    /// A call was refused.
    fn on_rejected(&self, _event: &CallRejected<'_>) {}
}

impl<T: CircuitBreakerObserver + ?Sized> CircuitBreakerObserver for Arc<T> {
    fn on_attach(&self, event: &Lifecycle<'_>) {
        (**self).on_attach(event);
    }

    fn on_detach(&self, event: &Lifecycle<'_>) {
        (**self).on_detach(event);
    }

    fn on_state_change(&self, event: &StateChange<'_>) {
        (**self).on_state_change(event);
    }

    fn on_call(&self, event: &CallRecorded<'_>) {
        (**self).on_call(event);
    }

    fn on_rejected(&self, event: &CallRejected<'_>) {
        (**self).on_rejected(event);
    }
}

/// See [`CircuitBreakerObserver::on_attach`] and
/// [`on_detach`](CircuitBreakerObserver::on_detach).
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Lifecycle<'a> {
    /// The breaker's name, if it has one.
    pub name: Option<&'a Arc<str>>,
    /// The breaker's state at that moment.
    pub state: State,
}

impl<'a> Lifecycle<'a> {
    /// Build the event, e.g. to test an observer.
    #[must_use]
    pub fn new(name: Option<&'a Arc<str>>, state: State) -> Self {
        Self { name, state }
    }
}

/// See [`CircuitBreakerObserver::on_state_change`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct StateChange<'a> {
    /// The breaker's name, if it has one.
    pub name: Option<&'a Arc<str>>,
    /// The state before.
    pub from: State,
    /// The state now.
    pub to: State,
    /// Grows with every change of this breaker (wrapping), so an observer can
    /// tell a late event from a newer one. Starts over in a new breaker, even
    /// one with the same name.
    pub generation: u64,
}

impl<'a> StateChange<'a> {
    /// Build the event, e.g. to test an observer.
    #[must_use]
    pub fn new(name: Option<&'a Arc<str>>, from: State, to: State, generation: u64) -> Self {
        Self {
            name,
            from,
            to,
            generation,
        }
    }
}

/// See [`CircuitBreakerObserver::on_call`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CallRecorded<'a> {
    /// The breaker's name, if it has one.
    pub name: Option<&'a Arc<str>>,
    /// What the caller recorded.
    pub outcome: Outcome,
    /// Whether the outcome reached the breaker's counts. `false` for
    /// [`Outcome::Ignore`] and for a result that arrived after the breaker had
    /// changed state.
    pub counted: bool,
}

impl<'a> CallRecorded<'a> {
    /// Build the event, e.g. to test an observer.
    #[must_use]
    pub fn new(name: Option<&'a Arc<str>>, outcome: Outcome, counted: bool) -> Self {
        Self {
            name,
            outcome,
            counted,
        }
    }
}

/// See [`CircuitBreakerObserver::on_rejected`].
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CallRejected<'a> {
    /// The breaker's name, if it has one.
    pub name: Option<&'a Arc<str>>,
    /// [`State::Open`], or [`State::HalfOpen`] when every probe slot was taken.
    pub state: State,
    /// The wait the caller was told, as in
    /// [`Rejected::retry_after`](super::Rejected::retry_after).
    pub retry_after: Duration,
}

impl<'a> CallRejected<'a> {
    /// Build the event, e.g. to test an observer.
    #[must_use]
    pub fn new(name: Option<&'a Arc<str>>, state: State, retry_after: Duration) -> Self {
        Self {
            name,
            state,
            retry_after,
        }
    }
}
