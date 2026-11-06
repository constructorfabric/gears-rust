//! Shared OpenTelemetry shutdown for host and out-of-process bootstraps.

/// Flush compatibility shutdown hooks for OpenTelemetry tracing and metrics.
///
/// This delegates to the current telemetry shutdown helpers so callers can use a
/// single bootstrap-level function during graceful shutdown.
///
/// `shutdown_metrics`/`shutdown_tracing` drain the SDK's batch processor, which
/// blocks on exporting the final batch (real network I/O to the OTLP endpoint).
/// Run that on a blocking-pool thread so it never stalls a Tokio worker thread.
pub(super) async fn tracing_shutdown() {
    if let Err(e) = tokio::task::spawn_blocking(|| {
        crate::telemetry::init::shutdown_metrics();
        crate::telemetry::init::shutdown_tracing();
    })
    .await
    {
        tracing::warn!(error = %e, "Telemetry shutdown task panicked");
    }
}
