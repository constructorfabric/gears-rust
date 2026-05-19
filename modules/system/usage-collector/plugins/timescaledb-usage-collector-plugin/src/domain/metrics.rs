//! Plugin metrics output port.
use modkit_macros::domain_model;

/// Observable metrics port for the `TimescaleDB` storage plugin.
///
/// Implementations live in `src/infra/` (e.g. OpenTelemetry counters).
/// The domain client depends only on this trait for testability.
pub trait PluginMetrics: Send + Sync {
    fn record_ingestion_success(&self);
    fn record_ingestion_error(&self);
    fn record_ingestion_latency_ms(&self, elapsed_ms: f64);
    fn record_dedup(&self);
    fn record_schema_validation_error(&self);
    fn record_query_latency_ms(&self, query_type: &str, elapsed_ms: f64);
    fn record_query_success(&self, query_type: &str);
    fn record_query_error(&self, query_type: &str);
}

/// No-op metrics implementation for integration tests and fallback initialization.
///
/// Only constructed by the integration test crate (gated behind the
/// `integration` feature); the production wiring uses `OtelPluginMetrics`.
#[cfg_attr(not(feature = "integration"), allow(dead_code))]
#[domain_model]
pub struct NoopMetrics;

impl PluginMetrics for NoopMetrics {
    fn record_ingestion_success(&self) {}
    fn record_ingestion_error(&self) {}
    fn record_ingestion_latency_ms(&self, _elapsed_ms: f64) {}
    fn record_dedup(&self) {}
    fn record_schema_validation_error(&self) {}
    fn record_query_latency_ms(&self, _query_type: &str, _elapsed_ms: f64) {}
    fn record_query_success(&self, _query_type: &str) {}
    fn record_query_error(&self, _query_type: &str) {}
}
