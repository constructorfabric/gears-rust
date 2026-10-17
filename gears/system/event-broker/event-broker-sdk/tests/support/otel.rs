//! A live OpenTelemetry tracer for trace-propagation tests, wired like
//! production: `tracing` spans bridged through `tracing-opentelemetry`.

// Shared by several test binaries; each uses a subset.
#![allow(dead_code)]

use opentelemetry::trace::TracerProvider as _;
use tracing_subscriber::layer::SubscriberExt as _;

/// Install a thread-local subscriber whose spans carry real OTel span
/// contexts, plus the W3C propagator. Keep the guard alive for the test body;
/// `#[tokio::test]` runs on one thread, so the default reaches every task the
/// test drives on it.
#[must_use]
pub fn install() -> tracing::subscriber::DefaultGuard {
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
    let tracer = provider.tracer("event-broker-sdk-test");
    let subscriber =
        tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
    tracing::subscriber::set_default(subscriber)
}

/// The `traceparent` of `span`, read while it is entered.
pub fn traceparent_of(span: &tracing::Span) -> String {
    span.in_scope(toolkit_trace_context::current_traceparent)
        .expect("a span under the OTel subscriber has a traceparent")
}
