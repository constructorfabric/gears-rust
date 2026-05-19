//! OpenTelemetry-backed implementation of [`PluginMetrics`].

use opentelemetry::KeyValue;
use opentelemetry::global;
use opentelemetry::metrics::{Counter, Histogram};

use crate::domain::metrics::PluginMetrics;

pub struct OtelPluginMetrics {
    ingestion_total: Counter<u64>,
    ingestion_errors_total: Counter<u64>,
    ingestion_latency_ms: Histogram<f64>,
    dedup_total: Counter<u64>,
    schema_validation_errors_total: Counter<u64>,
    query_latency_ms: Histogram<f64>,
    query_total: Counter<u64>,
    query_errors_total: Counter<u64>,
}

impl Default for OtelPluginMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl OtelPluginMetrics {
    #[must_use]
    pub fn new() -> Self {
        let meter = global::meter("timescaledb-usage-collector-storage-plugin");
        Self {
            ingestion_total: meter.u64_counter("usage_ingestion_total").build(),
            ingestion_errors_total: meter.u64_counter("usage_ingestion_errors_total").build(),
            ingestion_latency_ms: meter.f64_histogram("usage_ingestion_latency_ms").build(),
            dedup_total: meter.u64_counter("usage_dedup_total").build(),
            schema_validation_errors_total: meter
                .u64_counter("usage_schema_validation_errors_total")
                .build(),
            query_latency_ms: meter.f64_histogram("usage_query_latency_ms").build(),
            query_total: meter.u64_counter("usage_query_total").build(),
            query_errors_total: meter.u64_counter("usage_query_errors_total").build(),
        }
    }
}

impl PluginMetrics for OtelPluginMetrics {
    fn record_ingestion_success(&self) {
        self.ingestion_total
            .add(1, &[KeyValue::new("status", "success")]);
    }

    fn record_ingestion_error(&self) {
        self.ingestion_errors_total.add(1, &[]);
    }

    fn record_ingestion_latency_ms(&self, elapsed_ms: f64) {
        self.ingestion_latency_ms.record(elapsed_ms, &[]);
    }

    fn record_dedup(&self) {
        self.dedup_total.add(1, &[]);
        self.ingestion_total
            .add(1, &[KeyValue::new("status", "dedup")]);
    }

    fn record_schema_validation_error(&self) {
        self.schema_validation_errors_total.add(1, &[]);
    }

    fn record_query_latency_ms(&self, query_type: &str, elapsed_ms: f64) {
        self.query_latency_ms.record(
            elapsed_ms,
            &[KeyValue::new("query_type", query_type.to_owned())],
        );
    }

    fn record_query_success(&self, query_type: &str) {
        self.query_total.add(
            1,
            &[
                KeyValue::new("query_type", query_type.to_owned()),
                KeyValue::new("status", "success"),
            ],
        );
    }

    fn record_query_error(&self, query_type: &str) {
        self.query_total.add(
            1,
            &[
                KeyValue::new("query_type", query_type.to_owned()),
                KeyValue::new("status", "error"),
            ],
        );
        self.query_errors_total
            .add(1, &[KeyValue::new("query_type", query_type.to_owned())]);
    }
}
