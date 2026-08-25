//! Metrics tests: export the recorded instruments through an in-memory reader and
//! assert the real names, label sets, and values — a rename, a dropped label, or a
//! wrong value must all fail here.

use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData, ResourceMetrics};
use opentelemetry_sdk::metrics::in_memory_exporter::InMemoryMetricExporter;
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};

use super::{FetchMetrics, OtelFetchMetrics};

const PROVIDER: &str = "ecb";

/// Record one call against every instrument, then flush and return the export.
fn record_and_export() -> ResourceMetrics {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();

    let metrics = OtelFetchMetrics::new(&provider.meter("test"));
    metrics.observe_fetch(PROVIDER, 0.12);
    metrics.observe_rates_returned(PROVIDER, 31);
    metrics.incr_fetch_error(PROVIDER, "unreachable");
    metrics.incr_upstream_status(PROVIDER, 503);
    metrics.set_last_success(PROVIDER, 1_753_000_000);

    provider.force_flush().unwrap();
    exporter
        .get_finished_metrics()
        .unwrap()
        .pop()
        .expect("one export batch")
}

/// All instrument names present in the export.
fn instrument_names(rm: &ResourceMetrics) -> Vec<String> {
    rm.scope_metrics()
        .flat_map(|sm| sm.metrics().map(|m| m.name().to_owned()))
        .collect()
}

/// Every attribute `key=value` pair recorded for `instrument`.
fn attributes_of(rm: &ResourceMetrics, instrument: &str) -> Vec<String> {
    let mut out = Vec::new();
    for sm in rm.scope_metrics() {
        for m in sm.metrics().filter(|m| m.name() == instrument) {
            match m.data() {
                AggregatedMetrics::U64(MetricData::Sum(sum)) => {
                    for dp in sum.data_points() {
                        for kv in dp.attributes() {
                            out.push(format!("{}={}", kv.key, kv.value.as_str()));
                        }
                    }
                }
                AggregatedMetrics::I64(MetricData::Gauge(gauge)) => {
                    for dp in gauge.data_points() {
                        for kv in dp.attributes() {
                            out.push(format!("{}={}", kv.key, kv.value.as_str()));
                        }
                    }
                }
                AggregatedMetrics::F64(MetricData::Histogram(hist)) => {
                    for dp in hist.data_points() {
                        for kv in dp.attributes() {
                            out.push(format!("{}={}", kv.key, kv.value.as_str()));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

#[test]
fn exports_the_documented_prometheus_instrument_names() {
    // These names are the scrape contract dashboards and alerts are written
    // against (counters end `_total`, the duration histogram `_seconds`), so a
    // rename is a breaking change.
    let rm = record_and_export();
    let names = instrument_names(&rm);
    for expected in [
        "fx_provider_fetch_duration_seconds",
        "fx_provider_rates_returned",
        "fx_provider_fetch_errors_total",
        "fx_provider_last_success_timestamp",
        "fx_provider_upstream_status_total",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "missing instrument {expected}; exported: {names:?}"
        );
    }
}

#[test]
fn every_instrument_carries_the_provider_label() {
    let rm = record_and_export();
    for instrument in [
        "fx_provider_fetch_duration_seconds",
        "fx_provider_rates_returned",
        "fx_provider_fetch_errors_total",
        "fx_provider_last_success_timestamp",
        "fx_provider_upstream_status_total",
    ] {
        let attrs = attributes_of(&rm, instrument);
        assert!(
            attrs.iter().any(|a| a == "provider=ecb"),
            "{instrument} is missing the provider label; got {attrs:?}"
        );
    }
}

#[test]
fn fetch_errors_are_broken_down_by_kind() {
    // Without the `kind` label the error dashboard collapses into one line.
    let rm = record_and_export();
    let attrs = attributes_of(&rm, "fx_provider_fetch_errors_total");
    assert!(
        attrs.iter().any(|a| a == "kind=unreachable"),
        "expected kind=unreachable, got {attrs:?}"
    );
}

#[test]
fn upstream_statuses_are_broken_down_by_status() {
    let rm = record_and_export();
    let attrs = attributes_of(&rm, "fx_provider_upstream_status_total");
    assert!(
        attrs.iter().any(|a| a == "status=503"),
        "expected status=503, got {attrs:?}"
    );
}

#[test]
fn recorded_values_reach_the_exporter_unchanged() {
    let rm = record_and_export();
    let mut checked_counter = false;
    let mut checked_gauge = false;
    for sm in rm.scope_metrics() {
        for m in sm.metrics() {
            match (m.name(), m.data()) {
                ("fx_provider_fetch_errors_total", AggregatedMetrics::U64(MetricData::Sum(s))) => {
                    for dp in s.data_points() {
                        assert_eq!(dp.value(), 1, "one error was recorded");
                        checked_counter = true;
                    }
                }
                (
                    "fx_provider_last_success_timestamp",
                    AggregatedMetrics::I64(MetricData::Gauge(g)),
                ) => {
                    for dp in g.data_points() {
                        assert_eq!(
                            dp.value(),
                            1_753_000_000,
                            "the freshness gauge must export the exact unix time given"
                        );
                        checked_gauge = true;
                    }
                }
                _ => {}
            }
        }
    }
    assert!(checked_counter, "error counter was not exported");
    assert!(checked_gauge, "last-success gauge was not exported");
}

#[test]
fn from_global_records_without_panicking() {
    // The production constructor runs against the process-global provider, which
    // is a no-op until the host wires an exporter — safe to call at gear init().
    let m = OtelFetchMetrics::from_global();
    m.observe_fetch(PROVIDER, 0.12);
    m.incr_fetch_error(PROVIDER, "unreachable");
    m.incr_upstream_status(PROVIDER, 503);
    m.set_last_success(PROVIDER, 1_753_000_000);
    m.observe_rates_returned(PROVIDER, 31);
}
