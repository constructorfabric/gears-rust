//! The gate signal sink: the recording double captures every call, the `OTel` sink emits the
//! D-237 instruments with their labels, and the flow tokens and ceilings are the 03 §3.8 values.
//! In-crate: runs in the `orders-pg` CI lane with `gate-dev-stubs`.
#![allow(
    clippy::unwrap_used,
    reason = "test code: a failed metrics flush or collection must stop the test at once"
)]
use crate::domain::gate::breaker::BreakerState;
use crate::domain::gate::ports::{GatePort, Unavailable};
use crate::domain::gate::signals::RunSignals;
use crate::infra::gate::budget::CallOutcome;
use crate::infra::gate::signals::{
    Flow, GateObservation, GateSignals, NoGateSignals, OtelGateSignals, RecordingGateSignals,
};
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use std::collections::BTreeSet;
use std::sync::OnceLock;
use std::time::Duration;
use uuid::Uuid;

/// The test lock that serializes every test touching the process-global meter provider.
/// Under plain `cargo test` the tests are threads of one process, so one test swapping the
/// global between another's install and sink build would record into the wrong exporter.
/// Async tests take it with `.lock().await`, sync tests with `.blocking_lock()`, and hold the
/// guard for their whole run.
pub(super) fn meter_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

/// Install an in-memory meter provider as the process global, so the instruments
/// [`OtelGateSignals::new`] builds on `global::meter("bss-orders")` record into it (the
/// api-gateway `http_metrics_tests` precedent). Call it before building the sink, while
/// holding the [`meter_lock`] guard, which is taken as proof the global is the caller's.
pub(super) fn install_meter(
    _guard: &tokio::sync::MutexGuard<'_, ()>,
) -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    opentelemetry::global::set_meter_provider(provider.clone());
    (provider, exporter)
}

/// One exported `u64` data point: `(instrument, sorted attributes, value)`, the value being a
/// counter's sum, a gauge's last value or a histogram's count.
pub(super) type Point = (String, Vec<(String, String)>, u64);

/// Every `u64` data point of a fresh flush. The exporter keeps each flush's batch, so it is
/// cleared first: with the default cumulative temporality the new batch holds every point.
pub(super) fn exported(
    provider: &SdkMeterProvider,
    exporter: &InMemoryMetricExporter,
) -> BTreeSet<Point> {
    exporter.reset();
    provider.force_flush().unwrap();
    let mut points = BTreeSet::new();
    for rm in &exporter.get_finished_metrics().unwrap() {
        for sm in rm.scope_metrics() {
            for metric in sm.metrics() {
                let AggregatedMetrics::U64(data) = metric.data() else {
                    continue;
                };
                let name = metric.name().to_owned();
                let attrs = |kvs: &mut dyn Iterator<Item = &opentelemetry::KeyValue>| {
                    let mut v: Vec<(String, String)> = kvs
                        .map(|kv| (kv.key.to_string(), kv.value.to_string()))
                        .collect();
                    v.sort();
                    v
                };
                match data {
                    MetricData::Sum(sum) => {
                        for dp in sum.data_points() {
                            points.insert((name.clone(), attrs(&mut dp.attributes()), dp.value()));
                        }
                    }
                    MetricData::Gauge(gauge) => {
                        for dp in gauge.data_points() {
                            points.insert((name.clone(), attrs(&mut dp.attributes()), dp.value()));
                        }
                    }
                    MetricData::Histogram(hist) => {
                        for dp in hist.data_points() {
                            points.insert((name.clone(), attrs(&mut dp.attributes()), dp.count()));
                        }
                    }
                    MetricData::ExponentialHistogram(_) => {}
                }
            }
        }
    }
    points
}

/// The D-237 gate instruments [`OtelGateSignals::new`] builds. Other `bss-orders` instruments
/// (throttle, read, workers, gear) share the global meter in the same process, so an exact-set
/// assertion first keeps only these names.
const D233_INSTRUMENTS: [&str; 16] = [
    "orders_gate_port_calls_total",
    "orders_gate_port_call_duration_ms",
    "orders_gate_port_retries_total",
    "orders_gate_breaker_state",
    "orders_gate_breaker_transitions_total",
    "orders_gate_port_unavailable_total",
    "orders_gate_catalog_denials_total",
    "orders_gate_runs_total",
    "orders_gate_refusals_total",
    "orders_gate_predicates_unevaluable_total",
    "orders_gate_runs_unevaluable_total",
    "orders_gate_pins_total",
    "orders_gate_occupancy_incomplete_total",
    "orders_gate_resolution_duration_ms",
    "orders_gate_resolution_over_ceiling_total",
    "orders_gate_requests_total",
];

/// The exported points of the D-237 gate instruments only.
fn gate_points(points: BTreeSet<Point>) -> BTreeSet<Point> {
    points
        .into_iter()
        .filter(|(name, ..)| D233_INSTRUMENTS.contains(&name.as_str()))
        .collect()
}

/// An expected [`Point`].
pub(super) fn point(name: &str, attrs: &[(&str, &str)], value: u64) -> Point {
    let mut attrs: Vec<(String, String)> = attrs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect();
    attrs.sort();
    (name.to_owned(), attrs, value)
}

#[test]
fn the_recording_sink_records_each_call_in_order() {
    let sink = RecordingGateSignals::default();
    let tenant = Uuid::from_u128(0x5e11);
    let signals = RunSignals {
        blocked: true,
        pins_unresolvable: 1,
        ..RunSignals::default()
    };
    sink.request(Flow::Preview);
    sink.port_call(
        GatePort::Resolve,
        CallOutcome::Ok,
        Some(Duration::from_millis(3)),
    );
    sink.port_call(GatePort::Resolve, CallOutcome::BreakerOpen, None);
    sink.port_retry(GatePort::Resolve);
    sink.breaker_initial(GatePort::Assessment, BreakerState::Closed);
    sink.breaker(GatePort::Resolve, BreakerState::Open);
    sink.port_unavailable(GatePort::Assessment, Unavailable::Outage);
    sink.catalog_denial(tenant);
    sink.run(Flow::Submit, &signals, Duration::from_millis(7));
    assert_eq!(
        sink.observations(),
        vec![
            GateObservation::Request {
                flow: Flow::Preview
            },
            GateObservation::PortCall {
                port: GatePort::Resolve,
                outcome: CallOutcome::Ok,
                timed: true,
            },
            GateObservation::PortCall {
                port: GatePort::Resolve,
                outcome: CallOutcome::BreakerOpen,
                timed: false,
            },
            GateObservation::PortRetry {
                port: GatePort::Resolve,
            },
            GateObservation::BreakerInitial {
                port: GatePort::Assessment,
                state: BreakerState::Closed,
            },
            GateObservation::Breaker {
                port: GatePort::Resolve,
                state: BreakerState::Open,
            },
            GateObservation::PortUnavailable {
                port: GatePort::Assessment,
                kind: Unavailable::Outage,
            },
            GateObservation::CatalogDenial {
                catalog_tenant: tenant,
            },
            GateObservation::Run {
                flow: Flow::Submit,
                signals,
            },
        ]
    );
}

#[test]
fn the_flow_ceilings_are_the_run_level_budgets() {
    assert_eq!(Flow::Preview.ceiling(), Duration::from_millis(2500));
    assert_eq!(Flow::Submit.ceiling(), Duration::from_millis(2250));
}

#[test]
fn the_flow_tokens_are_the_label_values() {
    assert_eq!(Flow::Preview.token(), "preview");
    assert_eq!(Flow::Submit.token(), "submit");
}

/// Every D-237 instrument the calls below touch is exported under its name with exactly its
/// labels: `port`/`outcome` on calls (no seller attribute), `flow` on runs, and the catalog
/// tenant only on the catalog denial counter. Only the Preview run past its 2500 ms ceiling is
/// counted over the ceiling; the instant Submit run is not.
#[test]
fn the_otel_sink_emits_the_d233_instruments() {
    let guard = meter_lock().blocking_lock();
    let (provider, exporter) = install_meter(&guard);
    let sink = OtelGateSignals::new();
    let tenant = Uuid::from_u128(0x5e11);
    let signals = RunSignals {
        occupancy_incomplete: true,
        ..RunSignals::default()
    };
    sink.request(Flow::Submit);
    sink.port_call(
        GatePort::SkuRead,
        CallOutcome::Deadline,
        Some(Duration::from_millis(250)),
    );
    sink.port_call(GatePort::SkuRead, CallOutcome::Backpressure, None);
    sink.port_retry(GatePort::SkuRead);
    sink.breaker_initial(GatePort::SkuRead, BreakerState::Closed);
    sink.breaker(GatePort::SkuRead, BreakerState::HalfOpen);
    sink.port_unavailable(GatePort::Assessment, Unavailable::Outage);
    sink.catalog_denial(tenant);
    sink.run(Flow::Preview, &signals, Duration::from_secs(3));
    sink.run(Flow::Submit, &RunSignals::default(), Duration::ZERO);

    let sku = ("port", "sku-read");
    let expected: BTreeSet<Point> = [
        point("orders_gate_requests_total", &[("flow", "submit")], 1),
        point(
            "orders_gate_port_calls_total",
            &[sku, ("outcome", "deadline")],
            1,
        ),
        point(
            "orders_gate_port_calls_total",
            &[sku, ("outcome", "backpressure")],
            1,
        ),
        // Only the timed call (the deadline) is in the latency histogram.
        point("orders_gate_port_call_duration_ms", &[sku], 1),
        point("orders_gate_port_retries_total", &[sku], 1),
        // Half-open is gauge value 1; the initial state is not a transition.
        point("orders_gate_breaker_state", &[sku], 1),
        point(
            "orders_gate_breaker_transitions_total",
            &[sku, ("to", "half-open")],
            1,
        ),
        point(
            "orders_gate_port_unavailable_total",
            &[("port", "assessment"), ("kind", "outage")],
            1,
        ),
        point(
            "orders_gate_catalog_denials_total",
            &[("catalog_tenant", &tenant.to_string())],
            1,
        ),
        point(
            "orders_gate_runs_total",
            &[("flow", "preview"), ("outcome", "passed")],
            1,
        ),
        point(
            "orders_gate_runs_total",
            &[("flow", "submit"), ("outcome", "passed")],
            1,
        ),
        point(
            "orders_gate_occupancy_incomplete_total",
            &[("flow", "preview")],
            1,
        ),
        point(
            "orders_gate_resolution_duration_ms",
            &[("flow", "preview")],
            1,
        ),
        point(
            "orders_gate_resolution_duration_ms",
            &[("flow", "submit")],
            1,
        ),
        point(
            "orders_gate_resolution_over_ceiling_total",
            &[("flow", "preview")],
            1,
        ),
    ]
    .into_iter()
    .collect();
    assert_eq!(gate_points(exported(&provider, &exporter)), expected);

    // The no-op sink answers every call too.
    NoGateSignals.breaker_initial(GatePort::SkuRead, BreakerState::Closed);
    NoGateSignals.run(Flow::Submit, &signals, Duration::ZERO);
}

#[test]
fn unavailable_and_origin_tokens_are_the_label_values() {
    use crate::domain::gate::predicates::Origin;
    assert_eq!(
        [
            Unavailable::Outage,
            Unavailable::Denied,
            Unavailable::Deadline,
            Unavailable::Malformed,
            Unavailable::Missing,
        ]
        .map(Unavailable::token),
        ["outage", "denied", "deadline", "malformed", "missing"]
    );
    assert_eq!(
        [Origin::Adopted, Origin::Delta, Origin::Pin].map(Origin::token),
        ["adopted", "delta", "pin"]
    );
}

#[test]
fn outcome_tokens_are_the_d233_set() {
    assert_eq!(
        CallOutcome::ALL.map(CallOutcome::token),
        [
            "ok",
            "deadline",
            "transient",
            "permanent",
            "backpressure",
            "cancelled",
            "subrequest_cap",
            "breaker_open",
        ]
    );
}
