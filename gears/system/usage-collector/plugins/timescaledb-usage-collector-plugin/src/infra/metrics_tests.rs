use super::{ErrorClass, InsertMode, Metrics, QueryKind, label};

use opentelemetry::metrics::MeterProvider;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use sqlx::postgres::PgPoolOptions;

/// A lazy pool (`connect_lazy`) yields a `PgPool` handle without opening a
/// connection, so the metrics tests stay pure no-DB unit tests. `connect_lazy`
/// spawns the pool's background maintenance task, which is why these are
/// `#[tokio::test]` (a Tokio context is required; no DB connection is opened).
fn lazy_pool() -> sqlx::PgPool {
    PgPoolOptions::new()
        .connect_lazy("postgres://user:pass@localhost/db")
        .expect("a syntactically valid DSN yields a lazy pool without connecting")
}

/// A local `SdkMeterProvider` backed by an in-memory exporter. Local (not the
/// process-global) provider so the recording assertions are parallel-safe:
/// [`Metrics::with_meter`] takes the meter explicitly, so this test never
/// mutates `opentelemetry::global` state.
fn local_provider() -> (SdkMeterProvider, InMemoryMetricExporter) {
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    (provider, exporter)
}

/// Total of all `u64` Sum (counter) data points named `name`.
fn counter_sum(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum();
                }
            }
        }
    }
    0
}

/// Total of the `u64` Sum (counter) data points named `name` carrying
/// `label_key == label_value`.
fn counter_sum_with_label(
    exporter: &InMemoryMetricExporter,
    name: &str,
    label_key: &str,
    label_value: &str,
) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Sum(sum)) = metric.data()
                {
                    return sum
                        .data_points()
                        .filter(|dp| {
                            dp.attributes().any(|kv| {
                                kv.key.as_str() == label_key && kv.value.as_str() == label_value
                            })
                        })
                        .map(opentelemetry_sdk::metrics::data::SumDataPoint::value)
                        .sum();
                }
            }
        }
    }
    0
}

/// Last value of the `u64` Gauge named `name`, if recorded.
fn gauge_last_u64(exporter: &InMemoryMetricExporter, name: &str) -> Option<u64> {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::U64(MetricData::Gauge(g)) = metric.data()
                {
                    return g
                        .data_points()
                        .next()
                        .map(opentelemetry_sdk::metrics::data::GaugeDataPoint::value);
                }
            }
        }
    }
    None
}

/// Total observation count across the `f64` Histogram data points named `name`.
fn histogram_count(exporter: &InMemoryMetricExporter, name: &str) -> u64 {
    let metrics = exporter.get_finished_metrics().unwrap();
    for resource_metrics in &metrics {
        for scope_metrics in resource_metrics.scope_metrics() {
            for metric in scope_metrics.metrics() {
                if metric.name() == name
                    && let AggregatedMetrics::F64(MetricData::Histogram(h)) = metric.data()
                {
                    return h
                        .data_points()
                        .map(opentelemetry_sdk::metrics::data::HistogramDataPoint::count)
                        .sum();
                }
            }
        }
    }
    0
}

/// The recording helpers not exercised by
/// [`recording_helpers_emit_expected_series`] — the stale-dedup counter, a
/// single-row insert, and an aggregated query — emit their expected series
/// through the [`Metrics::with_meter`] seam.
///
/// Also smoke-checks that [`Metrics::new`] builds the full instrument inventory
/// against the process-global provider without panicking. Recordings on that
/// handle are a safe no-op (no reader is installed in the test process), so they
/// cannot be asserted directly — only the local `with_meter` provider is read
/// back. Together with the sibling test, every recording helper now has a value
/// assertion, not just a "does not panic" check.
#[tokio::test]
async fn remaining_recording_helpers_emit_expected_series() {
    // Global-provider construction path (the production entry point): building
    // the full inventory and recording against it must not panic.
    let global = Metrics::new(lazy_pool());
    global.set_ready(true);
    global.set_catalog_size(0);
    global.inc_dedup_absorbed();
    global.inc_dedup_stale();
    global.record_insert(InsertMode::Single, 0.001);
    global.record_query(QueryKind::Aggregated, 0.002);
    global.inc_backend_error(ErrorClass::Transient);

    // Value assertions via a local in-memory reader.
    let (provider, exporter) = local_provider();
    let metrics = Metrics::with_meter(&provider.meter("uc.timescaledb"), lazy_pool());

    metrics.inc_dedup_stale();
    metrics.inc_dedup_stale();
    metrics.inc_batch_retry();
    metrics.inc_batch_retry();
    metrics.inc_batch_retry();
    metrics.record_insert(InsertMode::Single, 0.001);
    metrics.record_query(QueryKind::Aggregated, 0.002);

    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum(&exporter, "uc_timescaledb_dedup_stale_total"),
        2
    );
    assert_eq!(
        counter_sum(&exporter, "uc_timescaledb_batch_retries_total"),
        3,
    );
    assert_eq!(
        histogram_count(&exporter, "uc_timescaledb_insert_duration_seconds"),
        1,
    );
    assert_eq!(
        histogram_count(&exporter, "uc_timescaledb_query_duration_seconds"),
        1,
    );
}

/// With an in-memory reader installed, the recording helpers must emit the
/// expected counter / gauge / histogram series — covering a plain counter, a
/// label-split counter, both gauge kinds, and a histogram.
#[tokio::test]
async fn recording_helpers_emit_expected_series() {
    let (provider, exporter) = local_provider();
    let metrics = Metrics::with_meter(&provider.meter("uc.timescaledb"), lazy_pool());

    // Counter (plain): three absorbed-dedup increments accumulate to 3.
    metrics.inc_dedup_absorbed();
    metrics.inc_dedup_absorbed();
    metrics.inc_dedup_absorbed();

    // Counter (labelled): backend errors split by `error_category`.
    metrics.inc_backend_error(ErrorClass::Transient);
    metrics.inc_backend_error(ErrorClass::Transient);
    metrics.inc_backend_error(ErrorClass::Internal);

    // Gauges: last-value semantics.
    metrics.set_catalog_size(42);
    metrics.set_ready(true);

    // Histogram (labelled): two batch-insert observations.
    metrics.record_insert(InsertMode::Batch, 0.01);
    metrics.record_insert(InsertMode::Batch, 0.02);

    provider.force_flush().unwrap();

    assert_eq!(
        counter_sum(&exporter, "uc_timescaledb_dedup_absorbed_total"),
        3,
    );
    assert_eq!(
        counter_sum(&exporter, "uc_timescaledb_backend_errors_total"),
        3,
    );
    assert_eq!(
        counter_sum_with_label(
            &exporter,
            "uc_timescaledb_backend_errors_total",
            label::ERROR_CATEGORY,
            label::ERROR_CATEGORY_TRANSIENT,
        ),
        2,
    );
    assert_eq!(
        gauge_last_u64(&exporter, "uc_timescaledb_usage_type_catalog_size"),
        Some(42),
    );
    assert_eq!(gauge_last_u64(&exporter, "uc_timescaledb_ready"), Some(1));
    assert_eq!(
        histogram_count(&exporter, "uc_timescaledb_insert_duration_seconds"),
        2,
    );
}
