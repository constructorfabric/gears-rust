//! Telemetry utilities for OpenTelemetry integration
//!
//! This gear provides utilities for setting up and configuring
//! OpenTelemetry tracing layers for distributed tracing.

pub mod config;
pub mod init;
pub mod throttled_log;

pub use config::{
    Exporter, LogsCorrelation, MetricsConfig, OpenTelemetryConfig, OpenTelemetryResource, Sampler,
    TracingConfig,
};
#[cfg(feature = "otel")]
pub use init::init_tracing;
pub use init::{init_metrics_provider, shutdown_tracing};
pub use throttled_log::ThrottledLog;
