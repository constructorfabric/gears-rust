//! OpenTelemetry metrics for resilience patterns. Feature `otel`.
//!
//! Instruments are created from a [`Meter`]; nothing here builds an exporter.
//! The application's meter provider (OTLP in a gear) decides where they go.

use std::sync::Arc;

use opentelemetry::metrics::{Counter, Meter, UpDownCounter};
use opentelemetry::{KeyValue, global};

use super::{SCOPE, UNNAMED, attr, circuit_breaker as names};
use crate::circuit_breaker::{
    CallRecorded, CallRejected, CircuitBreakerObserver, Lifecycle, State, StateChange,
};

/// A [`CircuitBreakerObserver`] that records the metrics in
/// [`super::circuit_breaker`]. One instance may observe several breakers; the
/// breaker's name tells their series apart, so give breakers distinct names.
#[derive(Debug, Clone)]
pub struct OtelCircuitBreakerMetrics {
    calls: Counter<u64>,
    state: UpDownCounter<i64>,
    transitions: Counter<u64>,
}

impl OtelCircuitBreakerMetrics {
    /// Instruments from the global meter provider, scope [`SCOPE`].
    ///
    /// Takes the provider installed at the time of the call, so build it after
    /// the application has set up telemetry.
    #[must_use]
    pub fn global() -> Self {
        let scope = opentelemetry::InstrumentationScope::builder(SCOPE).build();
        Self::new(&global::meter_with_scope(scope))
    }

    /// Instruments from `meter`.
    #[must_use]
    pub fn new(meter: &Meter) -> Self {
        Self {
            calls: meter
                .u64_counter(names::CALLS)
                .with_unit("{call}")
                .with_description("Calls seen by a circuit breaker, by outcome.")
                .build(),
            state: meter
                .i64_up_down_counter(names::STATE)
                .with_unit("{breaker}")
                .with_description("Circuit breakers in each state.")
                .build(),
            transitions: meter
                .u64_counter(names::TRANSITIONS)
                .with_unit("{transition}")
                .with_description("Circuit breaker state changes.")
                .build(),
        }
    }

    fn move_state(&self, name: Option<&Arc<str>>, state: State, delta: i64) {
        self.state.add(
            delta,
            &[
                name_attr(name),
                KeyValue::new(attr::STATE, names::state::of(state)),
            ],
        );
    }
}

/// The breaker already holds its name as an `Arc<str>`; sharing it costs a
/// reference count, not an allocation per call.
fn name_attr(name: Option<&Arc<str>>) -> KeyValue {
    match name {
        Some(name) => KeyValue::new(attr::NAME, Arc::clone(name)),
        None => KeyValue::new(attr::NAME, UNNAMED),
    }
}

impl CircuitBreakerObserver for OtelCircuitBreakerMetrics {
    fn on_attach(&self, event: &Lifecycle<'_>) {
        self.move_state(event.name, event.state, 1);
    }

    fn on_detach(&self, event: &Lifecycle<'_>) {
        self.move_state(event.name, event.state, -1);
    }

    fn on_state_change(&self, event: &StateChange<'_>) {
        // Out-of-order delivery is harmless: the two updates commute.
        self.move_state(event.name, event.from, -1);
        self.move_state(event.name, event.to, 1);
        self.transitions.add(
            1,
            &[
                name_attr(event.name),
                KeyValue::new(attr::PREVIOUS_STATE, names::state::of(event.from)),
                KeyValue::new(attr::STATE, names::state::of(event.to)),
            ],
        );
    }

    fn on_call(&self, event: &CallRecorded<'_>) {
        // A result that came too late to count is reported as ignored, which
        // is what it was to the breaker.
        let outcome = if event.counted {
            names::outcome::of(event.outcome)
        } else {
            names::outcome::IGNORED
        };
        self.calls.add(
            1,
            &[name_attr(event.name), KeyValue::new(attr::OUTCOME, outcome)],
        );
    }

    fn on_rejected(&self, event: &CallRejected<'_>) {
        self.calls.add(
            1,
            &[
                name_attr(event.name),
                KeyValue::new(attr::OUTCOME, names::outcome::REJECTED),
            ],
        );
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use opentelemetry::metrics::MeterProvider;
    use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
    use opentelemetry_sdk::metrics::{InMemoryMetricExporter, SdkMeterProvider};

    use super::*;
    use crate::circuit_breaker::{CircuitBreaker, CircuitBreakerConfig, Outcome};

    struct Harness {
        provider: SdkMeterProvider,
        exporter: InMemoryMetricExporter,
        metrics: OtelCircuitBreakerMetrics,
    }

    impl Harness {
        fn new() -> Self {
            let exporter = InMemoryMetricExporter::default();
            let provider = SdkMeterProvider::builder()
                .with_periodic_exporter(exporter.clone())
                .build();
            let metrics = OtelCircuitBreakerMetrics::new(&provider.meter("test"));
            Self {
                provider,
                exporter,
                metrics,
            }
        }

        fn breaker_with(&self, name: Option<&str>, config: CircuitBreakerConfig) -> CircuitBreaker {
            let mut breaker = CircuitBreaker::new(config).unwrap();
            if let Some(name) = name {
                breaker = breaker.with_name(name);
            }
            breaker.with_observer(self.metrics.clone())
        }

        fn breaker(&self, name: Option<&str>) -> Arc<CircuitBreaker> {
            let config = CircuitBreakerConfig::default()
                .with_consecutive_failures(2)
                .with_open_for(Duration::from_secs(5));
            Arc::new(self.breaker_with(name, config))
        }

        /// The value of `metric` at the point whose attributes include all of
        /// `expected`, from the latest export.
        fn value(&self, metric: &str, expected: &[(&str, &str)]) -> Option<i64> {
            self.provider.force_flush().unwrap();
            let batches = self.exporter.get_finished_metrics().unwrap();
            let matches = |attrs: &mut dyn Iterator<Item = &KeyValue>| {
                let attrs: Vec<_> = attrs.collect();
                expected.iter().all(|(k, v)| {
                    attrs
                        .iter()
                        .any(|kv| kv.key.as_str() == *k && kv.value.to_string() == *v)
                })
            };
            let mut found = None;
            for sm in batches.iter().flat_map(ResourceMetrics::scope_metrics) {
                for m in sm.metrics().filter(|m| m.name() == metric) {
                    match m.data() {
                        AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                            for dp in sum.data_points() {
                                if matches(&mut dp.attributes()) {
                                    found = Some(i64::try_from(dp.value()).unwrap());
                                }
                            }
                        }
                        AggregatedMetrics::I64(MetricData::Sum(sum)) => {
                            for dp in sum.data_points() {
                                if matches(&mut dp.attributes()) {
                                    found = Some(dp.value());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            found
        }

        /// Breakers named `name` in `state`; a missing series is zero.
        fn in_state(&self, name: &str, state: &str) -> i64 {
            self.value(names::STATE, &[(attr::NAME, name), (attr::STATE, state)])
                .unwrap_or(0)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn calls_are_counted_by_outcome() {
        let h = Harness::new();
        let b = h.breaker(Some("orders->payments"));
        b.try_acquire().unwrap().success();
        b.try_acquire().unwrap().record(Outcome::Ignore);
        b.try_acquire().unwrap().failure();
        b.try_acquire().unwrap().failure(); // opens
        assert!(b.try_acquire().is_err());

        let name = (attr::NAME, "orders->payments");
        for (outcome, count) in [
            (names::outcome::SUCCESS, 1),
            (names::outcome::IGNORED, 1),
            (names::outcome::FAILURE, 2),
            (names::outcome::REJECTED, 1),
        ] {
            assert_eq!(
                h.value(names::CALLS, &[name, (attr::OUTCOME, outcome)]),
                Some(count),
                "{outcome}"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn each_breaker_counts_once_in_its_state() {
        let h = Harness::new();
        let b = h.breaker(Some("orders->payments"));
        let n = "orders->payments";
        assert_eq!(h.in_state(n, names::state::CLOSED), 1, "from the start");

        b.try_acquire().unwrap().failure();
        b.try_acquire().unwrap().failure();
        assert_eq!(h.in_state(n, names::state::OPEN), 1);
        assert_eq!(h.in_state(n, names::state::CLOSED), 0);

        tokio::time::advance(Duration::from_secs(5)).await;
        b.try_acquire().unwrap().success(); // half-open, then closed
        assert_eq!(h.in_state(n, names::state::CLOSED), 1);
        assert_eq!(h.in_state(n, names::state::OPEN), 0);
        assert_eq!(h.in_state(n, names::state::HALF_OPEN), 0);

        let transition = |from, to| {
            h.value(
                names::TRANSITIONS,
                &[
                    (attr::NAME, n),
                    (attr::PREVIOUS_STATE, from),
                    (attr::STATE, to),
                ],
            )
        };
        assert_eq!(
            transition(names::state::CLOSED, names::state::OPEN),
            Some(1)
        );
        assert_eq!(
            transition(names::state::OPEN, names::state::HALF_OPEN),
            Some(1)
        );
        assert_eq!(
            transition(names::state::HALF_OPEN, names::state::CLOSED),
            Some(1)
        );
    }

    /// A breaker dropped while open and rebuilt under the same name must not
    /// leave the name counted as open.
    #[tokio::test(start_paused = true)]
    async fn a_dropped_breaker_leaves_its_state() {
        let h = Harness::new();
        let b = h.breaker(Some("gear@10.0.0.1"));
        b.try_acquire().unwrap().failure();
        b.try_acquire().unwrap().failure();
        assert_eq!(h.in_state("gear@10.0.0.1", names::state::OPEN), 1);

        drop(b);
        let _rebuilt = h.breaker(Some("gear@10.0.0.1"));
        assert_eq!(h.in_state("gear@10.0.0.1", names::state::OPEN), 0);
        assert_eq!(h.in_state("gear@10.0.0.1", names::state::CLOSED), 1);
    }

    /// Events from concurrent calls arrive in any order; the state counts must
    /// still end on the breaker's actual state.
    #[test]
    fn state_counts_survive_concurrent_transitions() {
        let h = Harness::new();
        let b = Arc::new(
            h.breaker_with(
                Some("busy"),
                CircuitBreakerConfig::default()
                    .with_consecutive_failures(1)
                    .with_open_for(Duration::from_micros(50)),
            ),
        );
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let b = Arc::clone(&b);
                std::thread::spawn(move || {
                    for i in 0..2_000 {
                        if let Ok(permit) = b.try_acquire() {
                            if (i + t) % 3 == 0 {
                                permit.failure();
                            } else {
                                permit.success();
                            }
                        }
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }

        let now = names::state::of(b.state());
        let transitions = h
            .value(
                names::TRANSITIONS,
                &[(attr::NAME, "busy"), (attr::STATE, names::state::OPEN)],
            )
            .unwrap_or(0);
        assert!(transitions > 0, "the test did not exercise transitions");
        for state in [
            names::state::CLOSED,
            names::state::OPEN,
            names::state::HALF_OPEN,
        ] {
            assert_eq!(
                h.in_state("busy", state),
                i64::from(state == now),
                "{state} (breaker is {now})"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_late_result_is_reported_as_ignored() {
        let h = Harness::new();
        let b = h.breaker(Some("late"));
        let late = b.try_acquire().unwrap();
        b.try_acquire().unwrap().failure();
        b.try_acquire().unwrap().failure(); // opens
        late.failure();

        let name = (attr::NAME, "late");
        assert_eq!(
            h.value(
                names::CALLS,
                &[name, (attr::OUTCOME, names::outcome::FAILURE)]
            ),
            Some(2)
        );
        assert_eq!(
            h.value(
                names::CALLS,
                &[name, (attr::OUTCOME, names::outcome::IGNORED)]
            ),
            Some(1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unnamed_breaker_is_reported_as_unnamed() {
        let h = Harness::new();
        let b = h.breaker(None);
        b.try_acquire().unwrap().success();
        assert_eq!(
            h.value(
                names::CALLS,
                &[
                    (attr::NAME, UNNAMED),
                    (attr::OUTCOME, names::outcome::SUCCESS)
                ]
            ),
            Some(1)
        );
    }
}
