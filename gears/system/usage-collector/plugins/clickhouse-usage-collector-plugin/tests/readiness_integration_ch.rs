#![cfg(feature = "clickhouse")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! `ClickHouse`-backed integration test for the `uc_clickhouse_ready` gauge
//! lifecycle on the request path: a connectivity failure clears it, and the
//! next successful round-trip re-arms it, on one metric series. Requires
//! Docker.

mod common;

use std::sync::Arc;

use opentelemetry::metrics::MeterProvider as _;
use opentelemetry_sdk::metrics::data::{AggregatedMetrics, MetricData};
use opentelemetry_sdk::metrics::{InMemoryMetricExporter, PeriodicReader, SdkMeterProvider};
use usage_collector_sdk::UsageCollectorPluginError;
use uuid::Uuid;

use clickhouse_usage_collector_plugin::domain::ports::RecordStore;
use clickhouse_usage_collector_plugin::infra::metrics::Metrics;

/// Last recorded `uc_clickhouse_ready` value after a fresh flush.
///
/// The exporter keeps every flushed export, so earlier exports are dropped
/// first and only the current collection cycle is read.
fn ready_gauge(provider: &SdkMeterProvider, exporter: &InMemoryMetricExporter) -> Option<u64> {
    exporter.reset();
    provider.force_flush().expect("flush in-memory metrics");
    let metrics = exporter.get_finished_metrics().expect("collected metrics");
    for rm in &metrics {
        for sm in rm.scope_metrics() {
            for m in sm.metrics() {
                if m.name() == "uc_clickhouse_ready"
                    && let AggregatedMetrics::U64(MetricData::Gauge(g)) = m.data()
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

/// Clear-then-re-arm on a single `Metrics` inventory: a store over a dead port
/// drops the gauge to 0, then a store over the live container — sharing the
/// same inventory — brings it back to 1 with one successful `get`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires Docker (testcontainers)"]
async fn ch_readiness_clears_on_outage_and_rearms_on_next_success() {
    let Some(h) = common::bring_up_or_skip().await else {
        return;
    };

    // Local provider so the gauge read is parallel-safe (never touches
    // `opentelemetry::global`).
    let exporter = InMemoryMetricExporter::default();
    let provider = SdkMeterProvider::builder()
        .with_reader(PeriodicReader::builder(exporter.clone()).build())
        .build();
    // No series yet: the gauge's first value is whatever the request path
    // records, which is what this test is about.
    let metrics = Arc::new(Metrics::with_meter(&provider.meter("uc.clickhouse")));

    // Outage: every statement against port 1 fails with a connection error.
    let dead =
        common::record_store_with_metrics(common::unreachable_client(), Arc::clone(&metrics));
    dead.get(Uuid::from_u128(0x7001))
        .await
        .expect_err("a get against a dead port must fail");
    assert_eq!(
        ready_gauge(&provider, &exporter),
        Some(0),
        "a connectivity failure on the request path must clear the readiness gauge"
    );

    // Recovery: the next successful round-trip re-arms the same series. A
    // `get` of an absent id is a successful `SELECT` returning no row, which
    // the store reports as the typed `NotFound` — a backend answer, not an
    // outage.
    let live = common::record_store_with_metrics(h.client.clone(), Arc::clone(&metrics));
    let err = live
        .get(Uuid::from_u128(0x7002))
        .await
        .expect_err("no fixture was inserted under this id");
    assert!(
        matches!(err, UsageCollectorPluginError::UsageRecordNotFound { .. }),
        "an absent id on a live backend is UsageRecordNotFound, got {err:?}"
    );
    assert_eq!(
        ready_gauge(&provider, &exporter),
        Some(1),
        "a successful round-trip must re-arm the readiness gauge"
    );
}
