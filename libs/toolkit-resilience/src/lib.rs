#![cfg_attr(coverage_nightly, feature(coverage_attribute))]
//! Client-side resilience for calls to remote dependencies.
//!
//! [`circuit_breaker`] stops calling a backend that keeps failing at the
//! transport level and lets a few probes through once it may have recovered.
//! The core decides nothing about transports: the caller says which outcomes
//! are failures. With the `tower` feature the same breaker runs as a tower
//! middleware, so it can sit in a tonic channel or an HTTP client stack.
//!
//! [`metrics`] names the metrics of every pattern here under one scheme. A
//! breaker reports its events to observers; with the `otel` feature it can
//! report them as OpenTelemetry metrics.
//!
//! The generated contract clients in `toolkit-contract` put it in front of
//! every attempt when they are built through wiring; hand-written clients can
//! add the layer themselves.

pub mod circuit_breaker;
pub mod metrics;
