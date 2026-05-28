//! Domain-level output ports.
//!
//! These traits decouple domain services from concrete infrastructure
//! implementations (OpenTelemetry, HTTP clients, …). Implementations live
//! under `crate::infra`.

pub(crate) mod metric_labels;
pub(crate) mod metrics;

#[allow(unused_imports)] // NoopMetrics is referenced only by test/test-utils builds
pub(crate) use metrics::{NoopMetrics, OagwMetricsPort};
