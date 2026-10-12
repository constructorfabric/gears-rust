//! OpenTelemetry-backed implementation of the domain-owned `FileStorageMetricsPort`.
//!
//! `FileStorageMetricsMeter` is used by both the control-plane gear and the sidecar binary,
//! each with its own `Meter` (separate OS processes).
//!
//! Counter names omit the `_total` suffix: the `opentelemetry-prometheus` exporter appends it.

use opentelemetry::KeyValue;
use opentelemetry::metrics::{Counter, Histogram, Meter};

use crate::domain::ports::FileStorageMetricsPort;

/// OpenTelemetry-backed `FileStorageMetricsPort`.
pub struct FileStorageMetricsMeter {
    operation: Counter<u64>,
    backend_error: Counter<u64>,
    quota_denied: Counter<u64>,
    sweep_abandoned_pending_deleted: Counter<u64>,
    sweep_abandoned_files_deleted: Counter<u64>,
    sweep_expired_multipart_aborted: Counter<u64>,
    sweep_retention_expired_deleted: Counter<u64>,
    sweep_idempotency_keys_deleted: Counter<u64>,
    ingress_bytes: Histogram<f64>,
    egress_bytes: Histogram<f64>,
    request_duration_ms: Histogram<f64>,
}

impl FileStorageMetricsMeter {
    /// Create all instruments; `prefix` (e.g. `"file_storage"`) is prepended to every name.
    #[must_use]
    pub fn new(meter: &Meter, prefix: &str) -> Self {
        Self {
            operation: meter
                .u64_counter(format!("{prefix}_operation"))
                .with_description("File-storage service-entry-point outcomes (op, result)")
                .build(),
            backend_error: meter
                .u64_counter(format!("{prefix}_backend_error"))
                .with_description("Storage-backend operation failures (backend_id, op)")
                .build(),
            quota_denied: meter
                .u64_counter(format!("{prefix}_quota_denied"))
                .with_description("Quota-enforcement denials (op)")
                .build(),
            sweep_abandoned_pending_deleted: meter
                .u64_counter(format!("{prefix}_sweep_abandoned_pending_deleted"))
                .with_description("Abandoned pending version rows deleted by the cleanup sweep")
                .build(),
            sweep_abandoned_files_deleted: meter
                .u64_counter(format!("{prefix}_sweep_abandoned_files_deleted"))
                .with_description(
                    "Permanent zero-version orphan `files` rows deleted by the cleanup sweep",
                )
                .build(),
            sweep_expired_multipart_aborted: meter
                .u64_counter(format!("{prefix}_sweep_expired_multipart_aborted"))
                .with_description(
                    "Expired in-progress multipart sessions aborted by the cleanup sweep",
                )
                .build(),
            sweep_retention_expired_deleted: meter
                .u64_counter(format!("{prefix}_sweep_retention_expired_deleted"))
                .with_description("Files deleted by the cleanup sweep due to a retention rule")
                .build(),
            sweep_idempotency_keys_deleted: meter
                .u64_counter(format!("{prefix}_sweep_idempotency_keys_deleted"))
                .with_description("Expired idempotency-key rows deleted by the cleanup sweep")
                .build(),
            ingress_bytes: meter
                .f64_histogram(format!("{prefix}_ingress_bytes"))
                .with_description("Bytes received per client upload (sidecar)")
                .build(),
            egress_bytes: meter
                .f64_histogram(format!("{prefix}_egress_bytes"))
                .with_description("Bytes served per client download (sidecar)")
                .build(),
            request_duration_ms: meter
                .f64_histogram(format!("{prefix}_sidecar_request_duration_ms"))
                .with_description(
                    "Sidecar HTTP request duration in milliseconds (route, method, status)",
                )
                .build(),
        }
    }
}

impl FileStorageMetricsPort for FileStorageMetricsMeter {
    fn record_operation(&self, op: &str, result: &str) {
        self.operation.add(
            1,
            &[
                KeyValue::new("op", op.to_owned()),
                KeyValue::new("result", result.to_owned()),
            ],
        );
    }

    fn record_backend_error(&self, backend_id: &str, op: &str) {
        self.backend_error.add(
            1,
            &[
                KeyValue::new("backend_id", backend_id.to_owned()),
                KeyValue::new("op", op.to_owned()),
            ],
        );
    }

    fn record_quota_denied(&self, op: &str) {
        self.quota_denied
            .add(1, &[KeyValue::new("op", op.to_owned())]);
    }

    fn record_sweep_result(
        &self,
        abandoned_pending_deleted: u64,
        abandoned_files_deleted: u64,
        expired_multipart_aborted: u64,
        retention_expired_deleted: u64,
        idempotency_keys_deleted: u64,
    ) {
        self.sweep_abandoned_pending_deleted
            .add(abandoned_pending_deleted, &[]);
        self.sweep_abandoned_files_deleted
            .add(abandoned_files_deleted, &[]);
        self.sweep_expired_multipart_aborted
            .add(expired_multipart_aborted, &[]);
        self.sweep_retention_expired_deleted
            .add(retention_expired_deleted, &[]);
        self.sweep_idempotency_keys_deleted
            .add(idempotency_keys_deleted, &[]);
    }

    fn record_ingress_bytes(&self, bytes: f64) {
        self.ingress_bytes.record(bytes, &[]);
    }

    fn record_egress_bytes(&self, bytes: f64) {
        self.egress_bytes.record(bytes, &[]);
    }

    fn record_request(&self, route: &str, method: &str, status: u16, latency_ms: f64) {
        self.request_duration_ms.record(
            latency_ms,
            &[
                KeyValue::new("route", route.to_owned()),
                KeyValue::new("method", method.to_owned()),
                KeyValue::new("status", i64::from(status)),
            ],
        );
    }
}

/// No-op `FileStorageMetricsPort`, the default for the services and `SidecarState`;
/// production wiring opts into `FileStorageMetricsMeter`.
pub struct NoopMetrics;

impl FileStorageMetricsPort for NoopMetrics {
    fn record_operation(&self, _op: &str, _result: &str) {}
    fn record_backend_error(&self, _backend_id: &str, _op: &str) {}
    fn record_quota_denied(&self, _op: &str) {}
    fn record_sweep_result(
        &self,
        _abandoned_pending_deleted: u64,
        _abandoned_files_deleted: u64,
        _expired_multipart_aborted: u64,
        _retention_expired_deleted: u64,
        _idempotency_keys_deleted: u64,
    ) {
    }
    fn record_ingress_bytes(&self, _bytes: f64) {}
    fn record_egress_bytes(&self, _bytes: f64) {}
    fn record_request(&self, _route: &str, _method: &str, _status: u16, _latency_ms: f64) {}
}
