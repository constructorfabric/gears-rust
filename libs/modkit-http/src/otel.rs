//! OpenTelemetry trace context helpers for HTTP headers
//!
//! Provides W3C Trace Context propagation with optional OpenTelemetry integration.
//! - With `otel` feature: Uses proper OTEL propagators for distributed tracing
//! - Without `otel` feature: No-op implementations (graceful degradation)

use http::HeaderMap;

/// W3C Trace Context header name
pub const TRACEPARENT: &str = "traceparent";

/// Extract traceparent header value from HTTP headers
#[must_use]
pub fn get_traceparent(headers: &HeaderMap) -> Option<&str> {
    headers.get(TRACEPARENT)?.to_str().ok()
}

/// Parse trace ID from W3C traceparent header (format: "00-{trace_id}-{span_id}-{flags}")
#[must_use]
pub fn parse_trace_id(traceparent: &str) -> Option<String> {
    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() >= 4 && parts[0] == "00" {
        Some(parts[1].to_owned())
    } else {
        None
    }
}

#[cfg(feature = "otel")]
mod imp {
    use super::{get_traceparent, parse_trace_id};
    use http::{HeaderMap, HeaderName, HeaderValue};
    use opentelemetry::{
        Context, global,
        propagation::{Extractor, Injector},
    };
    use tracing::Span;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    /// Adapter for extracting W3C Trace Context from HTTP headers
    struct HeadersExtractor<'a>(&'a HeaderMap);

    impl Extractor for HeadersExtractor<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0.get(key).and_then(|v| v.to_str().ok())
        }

        fn keys(&self) -> Vec<&str> {
            self.0.keys().map(http::HeaderName::as_str).collect()
        }
    }

    /// Adapter for injecting W3C Trace Context into HTTP headers
    struct HeadersInjector<'a>(&'a mut HeaderMap);

    impl Injector for HeadersInjector<'_> {
        fn set(&mut self, key: &str, value: String) {
            if let Ok(name) = HeaderName::from_bytes(key.as_bytes())
                && let Ok(val) = HeaderValue::from_str(&value)
            {
                self.0.insert(name, val);
            }
        }
    }

    /// Inject current OpenTelemetry context into HTTP headers.
    /// Uses the global propagator to inject W3C Trace Context.
    pub fn inject_current_span(headers: &mut HeaderMap) {
        let cx = Context::current();
        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&cx, &mut HeadersInjector(headers));
        });
    }

    /// Set span parent from W3C Trace Context headers.
    /// Extracts the trace context and sets it as the parent of the given span.
    pub fn set_parent_from_headers(span: &Span, headers: &HeaderMap) {
        // Extract parent context using OTEL propagator
        let parent_cx = global::get_text_map_propagator(|propagator| {
            propagator.extract(&HeadersExtractor(headers))
        });

        // Set as parent of current span
        _ = span.set_parent(parent_cx);

        // Also record trace IDs for log correlation
        if let Some(traceparent) = get_traceparent(headers)
            && let Some(trace_id) = parse_trace_id(traceparent)
        {
            span.record("trace_id", &trace_id);
            span.record("parent.trace_id", &trace_id);
        }
    }
}

#[cfg(not(feature = "otel"))]
mod imp {
    use super::{get_traceparent, parse_trace_id};
    use http::HeaderMap;
    use tracing::Span;

    /// No-op: OpenTelemetry is disabled
    pub fn inject_current_span(_headers: &mut HeaderMap) {
        // No-op when OTEL is disabled
    }

    /// No-op: OpenTelemetry is disabled
    /// Records trace IDs if present in headers for log correlation only.
    pub fn set_parent_from_headers(span: &Span, headers: &HeaderMap) {
        // Without OTEL, just record trace IDs for log correlation if present
        if let Some(traceparent) = get_traceparent(headers)
            && let Some(trace_id) = parse_trace_id(traceparent)
        {
            span.record("trace_id", &trace_id);
            span.record("parent.trace_id", &trace_id);
        }
    }
}

pub use imp::{inject_current_span, set_parent_from_headers};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "otel_tests.rs"]
mod tests;
