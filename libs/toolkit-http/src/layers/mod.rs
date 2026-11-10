//! Tower layers for HTTP client middleware
//!
//! This gear provides Tower service layers that can be composed to build
//! the HTTP client middleware stack.
//!
//! ## Available Layers
//!
//! - [`UserAgentLayer`] - Adds User-Agent header to all requests
//! - [`RetryLayer`] - Implements retry with exponential backoff and jitter
//! - [`OtelLayer`] - Adds OpenTelemetry tracing spans to outbound requests
//! - `MetricsLayer` - Records OpenTelemetry request-duration metrics (`otel` feature)
//! - [`SecureRedirectPolicy`] - Security-hardened redirect policy

#[cfg(feature = "otel")]
mod metrics;
mod otel;
mod redirect;
mod retry;
mod user_agent;

#[cfg(feature = "otel")]
pub use metrics::{
    ClassifyFn, MetricsLayer, MetricsService, RequestDurationRecorder, RequestLabels,
    RequestOutcome, default_classify,
};
pub use otel::{OtelLayer, OtelService};
pub use redirect::SecureRedirectPolicy;
pub use retry::{RETRY_ATTEMPT_HEADER, RetryLayer, RetryService};
pub use user_agent::{UserAgentLayer, UserAgentService};
