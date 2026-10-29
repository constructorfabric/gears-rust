//! What the gate reports (DESIGN 03 §3.8; D-233 item 3; D-223 naming): per port, every bounded
//! call's outcome and duration, its retry, the breaker's state and transitions, and an unavailable
//! answer by kind; per run, the outcome, the refused and unevaluable predicates, the pin and
//! occupancy signals and the resolution time against the flow's ceiling; per entry point, every
//! request (before the per-caller throttle). Instruments live on the process meter under
//! `bss-orders`; the trait keeps the gate testable with a recording sink.
//!
//! No instrument carries a tenant, subject or order attribute, except the catalog tenant on
//! `orders_gate_catalog_denials_total` (M3-4): a denied catalog is a per-seller configuration
//! fault an operator must be able to name.
use std::time::Duration;

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Gauge, Histogram};
use uuid::Uuid;

use super::budget::CallOutcome;
use crate::domain::gate::breaker::BreakerState;
use crate::domain::gate::ports::{GatePort, Unavailable};
use crate::domain::gate::signals::{PredicateSignal, RunSignals};

/// Which entry point a gate run serves: a Preview (indicative tax asked) or a submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// `PreviewService`: the run asks for indicative tax.
    Preview,
    /// `SubmitService`: phase A of a submit.
    Submit,
}

impl Flow {
    /// The `flow` label value.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Submit => "submit",
        }
    }

    /// The run-level resolution ceiling (03 §3.8): 2500 ms for a Preview, 2250 ms for a submit.
    #[must_use]
    pub const fn ceiling(self) -> Duration {
        match self {
            Self::Preview => Duration::from_millis(2500),
            Self::Submit => Duration::from_millis(2250),
        }
    }
}

/// The gate's signal sink.
pub trait GateSignals: Send + Sync + std::fmt::Debug {
    /// One bounded call on `port` ended with `outcome` (its label is [`CallOutcome::token`]:
    /// `ok`, `deadline`, `transient`, `permanent`, `backpressure`, `cancelled`, `subrequest_cap`,
    /// `breaker_open`). Every exit
    /// is counted. `duration` is `Some` (the whole call from entry, both attempts included) only
    /// when the call reached the callee, that is an attempt was started; it is `None` for an exit
    /// before the first attempt (an open breaker, backpressure, a cancellation or a deadline
    /// found already passed), so those near-zero exits do not dilute the latency histogram.
    ///
    /// `deadline` is one token for two situations: an admitted attempt that timed out, and a call
    /// that found the run's port deadline already elapsed (or passed it waiting for an in-run
    /// slot). Only the first has a `duration`; telling them apart takes that, not the token.
    ///
    /// `subrequest_cap` is untimed when the first attempt is refused by the cap; a cap hit on the
    /// retry is timed (the first attempt reached the callee).
    fn port_call(&self, port: GatePort, outcome: CallOutcome, duration: Option<Duration>);
    /// One bounded call on `port` took its second attempt.
    fn port_retry(&self, port: GatePort);
    /// The breaker of `port` changed to `state`: sets the state gauge and counts the transition.
    fn breaker(&self, port: GatePort, state: BreakerState);
    /// The breaker of `port` starts in `state` (construction): sets the state gauge only; it is
    /// not a transition and is not counted as one.
    fn breaker_initial(&self, port: GatePort, state: BreakerState);
    /// A run's `port` did not answer, for `kind` (once per port per run).
    fn port_unavailable(&self, port: GatePort, kind: Unavailable);
    /// A catalog port of a run was denied on `catalog_tenant`'s catalog.
    fn catalog_denial(&self, catalog_tenant: Uuid);
    /// A run of `flow` assessed its basket: its signals and its resolution time.
    fn run(&self, flow: Flow, signals: &RunSignals, elapsed: Duration);
    /// A request reached the `flow` entry point (before the per-caller throttle).
    fn request(&self, flow: Flow);
}

/// The sink that reports nothing (test composition roots and `Budget::baseline`).
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub struct NoGateSignals;

#[cfg(test)]
impl GateSignals for NoGateSignals {
    fn port_call(&self, _port: GatePort, _outcome: CallOutcome, _duration: Option<Duration>) {}
    fn port_retry(&self, _port: GatePort) {}
    fn breaker(&self, _port: GatePort, _state: BreakerState) {}
    fn breaker_initial(&self, _port: GatePort, _state: BreakerState) {}
    fn port_unavailable(&self, _port: GatePort, _kind: Unavailable) {}
    fn catalog_denial(&self, _catalog_tenant: Uuid) {}
    fn run(&self, _flow: Flow, _signals: &RunSignals, _elapsed: Duration) {}
    fn request(&self, _flow: Flow) {}
}

/// OpenTelemetry instruments on the process meter.
pub struct OtelGateSignals {
    port_calls: Counter<u64>,
    port_call_duration: Histogram<u64>,
    port_retries: Counter<u64>,
    breaker_state: Gauge<u64>,
    breaker_transitions: Counter<u64>,
    port_unavailable: Counter<u64>,
    catalog_denials: Counter<u64>,
    runs: Counter<u64>,
    refusals: Counter<u64>,
    predicates_unevaluable: Counter<u64>,
    runs_unevaluable: Counter<u64>,
    pins: Counter<u64>,
    occupancy_incomplete: Counter<u64>,
    resolution_duration: Histogram<u64>,
    over_ceiling: Counter<u64>,
    requests: Counter<u64>,
}

impl std::fmt::Debug for OtelGateSignals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtelGateSignals").finish_non_exhaustive()
    }
}

impl OtelGateSignals {
    /// Build the instruments on the `bss-orders` process meter.
    #[must_use]
    pub fn new() -> Self {
        let meter = opentelemetry::global::meter("bss-orders");
        Self {
            port_calls: meter
                .u64_counter("orders_gate_port_calls_total")
                .with_description("Bounded gate port calls by port and outcome")
                .build(),
            port_call_duration: meter
                .u64_histogram("orders_gate_port_call_duration_ms")
                .with_description("Bounded gate port call duration (both attempts) by port")
                .build(),
            port_retries: meter
                .u64_counter("orders_gate_port_retries_total")
                .with_description("Second attempts of bounded gate port calls by port")
                .build(),
            breaker_state: meter
                .u64_gauge("orders_gate_breaker_state")
                .with_description("Gate port breaker state by port (0 closed, 1 half-open, 2 open)")
                .build(),
            breaker_transitions: meter
                .u64_counter("orders_gate_breaker_transitions_total")
                .with_description("Gate port breaker transitions by port and to (target state)")
                .build(),
            port_unavailable: meter
                .u64_counter("orders_gate_port_unavailable_total")
                .with_description("Gate ports that did not answer a run, by port and kind")
                .build(),
            catalog_denials: meter
                .u64_counter("orders_gate_catalog_denials_total")
                .with_description("Catalog port denials by catalog_tenant (the seller)")
                .build(),
            runs: meter
                .u64_counter("orders_gate_runs_total")
                .with_description("Assessed gate runs by flow and outcome")
                .build(),
            refusals: meter
                .u64_counter("orders_gate_refusals_total")
                .with_description(
                    "Refused predicates per run by flow, predicate, origin and reason",
                )
                .build(),
            predicates_unevaluable: meter
                .u64_counter("orders_gate_predicates_unevaluable_total")
                .with_description(
                    "Unevaluable predicates per run by flow, predicate, origin and reason",
                )
                .build(),
            runs_unevaluable: meter
                .u64_counter("orders_gate_runs_unevaluable_total")
                .with_description("Gate runs with any unevaluable predicate by flow")
                .build(),
            pins: meter
                .u64_counter("orders_gate_pins_total")
                .with_description(
                    "Gate runs with a pin that did not compose, by flow and kind (unresolvable, unevaluable)",
                )
                .build(),
            occupancy_incomplete: meter
                .u64_counter("orders_gate_occupancy_incomplete_total")
                .with_description("Gate runs with an incomplete occupancy answer by flow")
                .build(),
            resolution_duration: meter
                .u64_histogram("orders_gate_resolution_duration_ms")
                .with_description("Gate run resolution time by flow")
                .build(),
            over_ceiling: meter
                .u64_counter("orders_gate_resolution_over_ceiling_total")
                .with_description("Gate runs whose resolution passed the flow's ceiling, by flow")
                .build(),
            requests: meter
                .u64_counter("orders_gate_requests_total")
                .with_description("Requests at the gate entry points (before the throttle) by flow")
                .build(),
        }
    }

    /// One predicate counter entry.
    fn predicate(counter: &Counter<u64>, flow: Flow, s: PredicateSignal) {
        counter.add(
            1,
            &[
                KeyValue::new("flow", flow.token()),
                KeyValue::new("predicate", s.predicate.token()),
                KeyValue::new("origin", s.origin.token()),
                KeyValue::new("reason", s.reason.token()),
            ],
        );
    }
}

impl Default for OtelGateSignals {
    fn default() -> Self {
        Self::new()
    }
}

/// Whole milliseconds, saturating.
fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

impl GateSignals for OtelGateSignals {
    fn port_call(&self, port: GatePort, outcome: CallOutcome, duration: Option<Duration>) {
        self.port_calls.add(
            1,
            &[
                KeyValue::new("port", port.token()),
                KeyValue::new("outcome", outcome.token()),
            ],
        );
        if let Some(duration) = duration {
            self.port_call_duration
                .record(millis(duration), &[KeyValue::new("port", port.token())]);
        }
    }

    fn port_retry(&self, port: GatePort) {
        self.port_retries
            .add(1, &[KeyValue::new("port", port.token())]);
    }

    fn breaker_initial(&self, port: GatePort, state: BreakerState) {
        self.breaker_state
            .record(state.gauge(), &[KeyValue::new("port", port.token())]);
    }

    fn breaker(&self, port: GatePort, state: BreakerState) {
        self.breaker_initial(port, state);
        self.breaker_transitions.add(
            1,
            &[
                KeyValue::new("port", port.token()),
                KeyValue::new("to", state.token()),
            ],
        );
    }

    fn port_unavailable(&self, port: GatePort, kind: Unavailable) {
        self.port_unavailable.add(
            1,
            &[
                KeyValue::new("port", port.token()),
                KeyValue::new("kind", kind.token()),
            ],
        );
    }

    fn catalog_denial(&self, catalog_tenant: Uuid) {
        self.catalog_denials.add(
            1,
            &[KeyValue::new("catalog_tenant", catalog_tenant.to_string())],
        );
    }

    fn run(&self, flow: Flow, signals: &RunSignals, elapsed: Duration) {
        let by_flow = [KeyValue::new("flow", flow.token())];
        let outcome = if signals.blocked { "blocked" } else { "passed" };
        self.runs.add(
            1,
            &[
                KeyValue::new("flow", flow.token()),
                KeyValue::new("outcome", outcome),
            ],
        );
        for s in &signals.refused {
            Self::predicate(&self.refusals, flow, *s);
        }
        for s in &signals.unevaluable {
            Self::predicate(&self.predicates_unevaluable, flow, *s);
        }
        if !signals.unevaluable.is_empty() {
            self.runs_unevaluable.add(1, &by_flow);
        }
        // Run-level (plan M3-3): one per kind per run whatever the run's line count for it.
        for (kind, count) in [
            ("unresolvable", signals.pins_unresolvable),
            ("unevaluable", signals.pins_unevaluable),
        ] {
            if count > 0 {
                self.pins.add(
                    1,
                    &[
                        KeyValue::new("flow", flow.token()),
                        KeyValue::new("kind", kind),
                    ],
                );
            }
        }
        if signals.occupancy_incomplete {
            self.occupancy_incomplete.add(1, &by_flow);
        }
        self.resolution_duration.record(millis(elapsed), &by_flow);
        if elapsed > flow.ceiling() {
            self.over_ceiling.add(1, &by_flow);
        }
    }

    fn request(&self, flow: Flow) {
        self.requests.add(1, &[KeyValue::new("flow", flow.token())]);
    }
}

/// One recorded call of [`RecordingGateSignals`] (tests only); durations are not recorded, only
/// whether a port call carried one.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateObservation {
    /// A [`GateSignals::port_call`].
    PortCall {
        /// The port.
        port: GatePort,
        /// The outcome.
        outcome: CallOutcome,
        /// Whether a duration was reported (the call reached the callee).
        timed: bool,
    },
    /// A [`GateSignals::port_retry`].
    PortRetry {
        /// The port.
        port: GatePort,
    },
    /// A [`GateSignals::breaker_initial`].
    BreakerInitial {
        /// The port.
        port: GatePort,
        /// The state it starts in.
        state: BreakerState,
    },
    /// A [`GateSignals::breaker`].
    Breaker {
        /// The port.
        port: GatePort,
        /// The state reported.
        state: BreakerState,
    },
    /// A [`GateSignals::port_unavailable`].
    PortUnavailable {
        /// The port.
        port: GatePort,
        /// Why it did not answer.
        kind: Unavailable,
    },
    /// A [`GateSignals::catalog_denial`].
    CatalogDenial {
        /// The denied catalog's tenant.
        catalog_tenant: Uuid,
    },
    /// A [`GateSignals::run`].
    Run {
        /// The flow.
        flow: Flow,
        /// The run's signals.
        signals: RunSignals,
    },
    /// A [`GateSignals::request`].
    Request {
        /// The flow.
        flow: Flow,
    },
}

/// A sink that records every call (tests only), the `RecordingMetrics` shape.
#[cfg(test)]
#[derive(Debug, Default)]
pub struct RecordingGateSignals {
    observations: parking_lot::Mutex<Vec<GateObservation>>,
}

#[cfg(test)]
impl RecordingGateSignals {
    /// Everything observed so far, in call order.
    #[must_use]
    pub fn observations(&self) -> Vec<GateObservation> {
        self.observations.lock().clone()
    }

    fn record(&self, observation: GateObservation) {
        self.observations.lock().push(observation);
    }
}

#[cfg(test)]
impl GateSignals for RecordingGateSignals {
    fn port_call(&self, port: GatePort, outcome: CallOutcome, duration: Option<Duration>) {
        self.record(GateObservation::PortCall {
            port,
            outcome,
            timed: duration.is_some(),
        });
    }
    fn port_retry(&self, port: GatePort) {
        self.record(GateObservation::PortRetry { port });
    }
    fn breaker_initial(&self, port: GatePort, state: BreakerState) {
        self.record(GateObservation::BreakerInitial { port, state });
    }
    fn breaker(&self, port: GatePort, state: BreakerState) {
        self.record(GateObservation::Breaker { port, state });
    }
    fn port_unavailable(&self, port: GatePort, kind: Unavailable) {
        self.record(GateObservation::PortUnavailable { port, kind });
    }
    fn catalog_denial(&self, catalog_tenant: Uuid) {
        self.record(GateObservation::CatalogDenial { catalog_tenant });
    }
    fn run(&self, flow: Flow, signals: &RunSignals, _elapsed: Duration) {
        self.record(GateObservation::Run {
            flow,
            signals: signals.clone(),
        });
    }
    fn request(&self, flow: Flow) {
        self.record(GateObservation::Request { flow });
    }
}
