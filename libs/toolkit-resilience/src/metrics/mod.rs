//! Metric names and attributes for every resilience pattern in this crate.
//!
//! One scheme, so a dashboard or alert written for one pattern reads the same
//! for the next:
//!
//! - Metric names are `resilience.<pattern>.<measurement>`, in the dotted
//!   OpenTelemetry style. Counters carry no `_total` or `.count` suffix (a
//!   Prometheus exporter adds `_total` itself); units are UCUM annotations
//!   (`{call}`, `{transition}`, `{breaker}`).
//! - State is an up-down counter split by state, one unit per instance, not a
//!   gauge holding a state code: updates commute, so it stays right when events
//!   from concurrent calls arrive out of order and under delta temporality, and
//!   it goes back to zero when an instance is dropped.
//! - Attributes shared by all patterns are in [`attr`]. A pattern-specific
//!   attribute is `resilience.<pattern>.<key>`.
//! - [`attr::NAME`] identifies the instance (one breaker, one retry policy). It
//!   must come from configuration, one per client and target, never from a
//!   request, or metric cardinality is unbounded. An instance without a name is
//!   reported as [`UNNAMED`].
//! - A new pattern adds a module here next to [`circuit_breaker`] and reuses
//!   [`attr`].
//!
//! The constants are always compiled. The OpenTelemetry instruments that use
//! them are behind the `otel` feature (`otel::OtelCircuitBreakerMetrics`).

#[cfg(feature = "otel")]
pub mod otel;

/// Instrumentation scope of the meter the `otel` feature uses.
pub const SCOPE: &str = "cf-gears-toolkit-resilience";

/// [`attr::NAME`] of an instance that was not given a name.
pub const UNNAMED: &str = "unnamed";

/// Attribute keys shared by every pattern.
pub mod attr {
    /// The instance: a breaker's name, set from configuration.
    pub const NAME: &str = "resilience.name";
    /// What a call came to; the values are per pattern.
    pub const OUTCOME: &str = "resilience.outcome";
    /// The instance's state, for patterns that have one.
    pub const STATE: &str = "resilience.state";
    /// The state before a transition.
    pub const PREVIOUS_STATE: &str = "resilience.previous_state";
}

/// [`crate::circuit_breaker`].
pub mod circuit_breaker {
    use crate::circuit_breaker::{Outcome, State};

    /// Calls seen by a breaker, by [`attr::NAME`](super::attr::NAME) and
    /// [`attr::OUTCOME`](super::attr::OUTCOME) (see [`outcome`]). Counter,
    /// `{call}`.
    pub const CALLS: &str = "resilience.circuit_breaker.calls";
    /// Breakers in each [`attr::STATE`](super::attr::STATE) (see [`state`]):
    /// each breaker counts `1` in its current state. Up-down counter,
    /// `{breaker}`. Summed over [`attr::NAME`](super::attr::NAME), it is the
    /// number of breakers in a state.
    pub const STATE: &str = "resilience.circuit_breaker.state";
    /// State changes, by [`attr::PREVIOUS_STATE`](super::attr::PREVIOUS_STATE)
    /// and [`attr::STATE`](super::attr::STATE). Counter, `{transition}`.
    pub const TRANSITIONS: &str = "resilience.circuit_breaker.transitions";

    /// Values of [`attr::OUTCOME`](super::attr::OUTCOME) on [`CALLS`].
    pub mod outcome {
        /// The backend answered.
        pub const SUCCESS: &str = "success";
        /// The call counted as a failure.
        pub const FAILURE: &str = "failure";
        /// The call said nothing about the backend, or its result came too
        /// late to count.
        pub const IGNORED: &str = "ignored";
        /// The breaker refused the call.
        pub const REJECTED: &str = "rejected";

        /// The value for a recorded outcome.
        #[must_use]
        pub const fn of(outcome: super::Outcome) -> &'static str {
            match outcome {
                super::Outcome::Success => SUCCESS,
                super::Outcome::Failure => FAILURE,
                super::Outcome::Ignore => IGNORED,
            }
        }
    }

    /// Values of [`attr::STATE`](super::attr::STATE) and
    /// [`attr::PREVIOUS_STATE`](super::attr::PREVIOUS_STATE).
    pub mod state {
        /// Calls go through.
        pub const CLOSED: &str = "closed";
        /// Calls are rejected.
        pub const OPEN: &str = "open";
        /// Probes go through.
        pub const HALF_OPEN: &str = "half_open";

        /// The value for a state.
        #[must_use]
        pub const fn of(state: super::State) -> &'static str {
            match state {
                super::State::Closed => CLOSED,
                super::State::Open => OPEN,
                super::State::HalfOpen => HALF_OPEN,
            }
        }
    }
}
