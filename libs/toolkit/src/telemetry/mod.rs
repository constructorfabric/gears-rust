//! Telemetry utilities for OpenTelemetry integration
//!
//! This gear provides utilities for setting up and configuring
//! OpenTelemetry tracing layers for distributed tracing.

pub mod config;
pub mod init;
pub mod throttled_log;
pub(crate) mod trace_context;

pub use config::{
    Exporter, HttpOpts, LogsCorrelation, MetricsConfig, OpenTelemetryConfig, OpenTelemetryResource,
    Propagation, Sampler, TracingConfig,
};
#[cfg(feature = "otel")]
pub use init::init_tracing;
pub use init::{init_metrics_provider, shutdown_tracing};
pub use throttled_log::ThrottledLog;
