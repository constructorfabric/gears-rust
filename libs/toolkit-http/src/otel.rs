//! OpenTelemetry trace context helpers for HTTP headers.
//!
//! This module is a thin facade over `cf-gears-toolkit-trace-context`, the one
//! crate that owns W3C Trace Context propagation (parsing, inbound span-parent
//! seeding, and outbound injection). `cf-gears-toolkit` depends on the same
//! crate, so this crate and the core toolkit cannot drift on what a valid
//! `traceparent` is or on how context is propagated.
//!
//! The names are re-exported here so existing `toolkit_http::otel::*` call sites
//! (e.g. the outgoing-request tracing layer and the api-gateway `TraceLayer`)
//! keep working unchanged.

pub use toolkit_trace_context::{
    TRACEPARENT, get_traceparent, inject_current_span, parse_trace_id, set_parent_from_headers,
};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use http::HeaderMap;
    use tracing::info_span;

    #[test]
    #[cfg(not(feature = "otel"))]
    fn test_inject_current_span_noop() {
        let mut headers = HeaderMap::new();
        inject_current_span(&mut headers);
        // Should be no-op, no headers added
        assert!(headers.is_empty());
    }

    #[test]
    #[cfg(feature = "otel")]
    fn test_inject_current_span_no_panic() {
        use opentelemetry::global;
        use opentelemetry_sdk::propagation::TraceContextPropagator;

        global::set_text_map_propagator(TraceContextPropagator::new());

        let mut headers = HeaderMap::new();
        let _span = tracing::info_span!("test").entered();
        // Without full OTEL setup, this may not inject anything, but shouldn't panic
        inject_current_span(&mut headers);
    }

    /// The re-exported inbound helpers stay reachable under this crate's own
    /// `toolkit_http::otel::*` path in both feature modes.
    #[test]
    fn test_set_parent_from_headers_no_panic() {
        let mut headers = HeaderMap::new();
        headers.insert(
            TRACEPARENT,
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"
                .parse()
                .expect("valid header"),
        );

        let span = info_span!(
            "test",
            trace_id = tracing::field::Empty,
            parent.trace_id = tracing::field::Empty
        );

        // Should not panic in either mode
        set_parent_from_headers(&span, &headers);
        assert_eq!(
            parse_trace_id("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01").as_deref(),
            Some("4bf92f3577b34da6a3ce929d0e0e4736")
        );
    }
}
