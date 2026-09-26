//! Reading the live OpenTelemetry trace/span ids from the current context.
//!
//! The JSON log-correlation formatter (`crate::bootstrap::host::log_correlation`)
//! and the canonical error layer (`crate::api::canonical_error_layer`) both need
//! the ids of the currently active `OTel` span and must resolve them the same
//! way, so the logic lives here once.
//!
//! It reads `OTel`'s own context directly rather than via
//! `OpenTelemetrySpanExt::context()`, which re-enters the subscriber and yields
//! nothing from inside `on_event`. `OpenTelemetryLayer` attaches the context on
//! span entry (`context_activation`, on by default), so it is already current
//! by the time these run.

/// The `(trace_id, span_id)` of the currently active `OTel` span, if valid.
///
/// Both ids render as lowercase hex (32 and 16 chars) via `Display`, matching
/// the `traceparent` wire form and `toolkit_http::otel::parse_trace_id`.
#[cfg(feature = "otel")]
#[must_use]
pub fn current_trace_ids() -> Option<(String, String)> {
    use opentelemetry::trace::TraceContextExt as _;

    let context = opentelemetry::Context::current();
    let span = context.span();
    let span_context = span.span_context();
    if !span_context.is_valid() {
        return None;
    }
    Some((
        span_context.trace_id().to_string(),
        span_context.span_id().to_string(),
    ))
}

/// The `trace_id` of the currently active `OTel` span, if valid.
#[cfg(feature = "otel")]
#[must_use]
pub fn current_trace_id() -> Option<String> {
    current_trace_ids().map(|(trace_id, _)| trace_id)
}

/// No `OTel` compiled in: there is never a span context to read.
#[cfg(not(feature = "otel"))]
#[must_use]
pub fn current_trace_id() -> Option<String> {
    None
}
