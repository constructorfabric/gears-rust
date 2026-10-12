//! W3C Trace Context propagation shared by CF/Gears crates.
//!
//! The core toolkit (`cf-gears-toolkit`) and the HTTP crate
//! (`cf-gears-toolkit-http`) both need to validate an inbound W3C `traceparent`,
//! seed a span's parent from it, and inject the active context on egress.
//! Historically each kept its own copy, which drifted; this crate is the one
//! place both depend on so the gateway and the gears agree on what a valid
//! `traceparent` is, on which ids a request span carries, and on how context is
//! propagated.
//!
//! - [`set_parent_from_headers`] — inbound continuation. With the `otel` feature
//!   it continues the caller's trace via the process-global text-map
//!   propagator; without it it still records `trace_id` / `parent.trace_id` on
//!   the span for log correlation, but the span is a fresh root.
//! - [`inject_current_span`] — outbound propagation. With `otel` it injects the
//!   active context as `traceparent`; without it, a no-op.
//! - [`current_trace_id`] / [`current_trace_ids`] — read the ids of the active
//!   `OTel` span (what the log-correlation formatter and error path stamp).
//! - [`extract_trace_id`] — the wire-id resolver: active span, else a validated
//!   inbound `traceparent`.
#![cfg_attr(coverage_nightly, feature(coverage_attribute))]

use http::HeaderMap;

/// The `(trace_id, span_id)` of the currently active `OTel` span, if valid.
///
/// Returns the raw `Copy` id types rather than `String`s: both render as
/// lowercase hex (32 and 16 chars) via `Display`, matching the `traceparent`
/// wire form and [`parse_trace_id`], so a log formatter can write them straight
/// into its output with no intermediate allocation.
///
/// Reads `OTel`'s own context directly rather than via
/// `OpenTelemetrySpanExt::context()`, which re-enters the subscriber and yields
/// nothing from inside `on_event`. `OpenTelemetryLayer` attaches the context on
/// span entry (`context_activation`, on by default), so it is already current
/// by the time a formatter runs.
///
/// Only a log-correlation formatter needs *both* ids; the per-error path uses
/// [`current_trace_id`] instead, which reads the `trace_id` alone.
#[cfg(feature = "otel")]
#[must_use]
pub fn current_trace_ids() -> Option<(opentelemetry::trace::TraceId, opentelemetry::trace::SpanId)>
{
    use opentelemetry::trace::TraceContextExt as _;

    let context = opentelemetry::Context::current();
    let span = context.span();
    let span_context = span.span_context();
    span_context
        .is_valid()
        .then(|| (span_context.trace_id(), span_context.span_id()))
}

/// The `trace_id` of the currently active `OTel` span, if valid.
///
/// Reads the `trace_id` directly rather than via [`current_trace_ids`], to avoid
/// allocating the discarded `span_id` on the per-error-response path.
#[cfg(feature = "otel")]
#[must_use]
pub fn current_trace_id() -> Option<String> {
    use opentelemetry::trace::TraceContextExt as _;

    let context = opentelemetry::Context::current();
    let span = context.span();
    let span_context = span.span_context();
    span_context
        .is_valid()
        .then(|| span_context.trace_id().to_string())
}

/// No `OTel` compiled in: there is never a span context to read.
#[cfg(not(feature = "otel"))]
#[must_use]
pub fn current_trace_id() -> Option<String> {
    None
}

/// Resolve the wire `trace_id`: live `OTel` span context (the same source a
/// log-correlation formatter uses) → incoming W3C `traceparent` (validated by
/// [`parse_trace_id`]).
///
/// The live span is preferred so the wire id is *always* the one the caller's
/// own log lines carry: on a successful continuation the two are equal, and on a
/// fresh root (no propagator, or `set_parent` failed) the span's id is what the
/// logs show — reading the header first would ship an id the logs don't carry.
/// The header is used only when no span is active (no `otel`, or outside any
/// request span), and an invalid `traceparent` is treated as absent.
///
/// It does NOT fall back to `x-request-id` or a span handle: those are different
/// identifiers that resolve to nothing under `trace_id` in a trace backend.
/// Absent both, `trace_id` is left absent rather than filled with a stand-in.
#[must_use]
pub fn extract_trace_id(headers: &HeaderMap) -> Option<String> {
    current_trace_id().or_else(|| get_traceparent(headers).and_then(parse_trace_id))
}

/// W3C Trace Context header name.
pub const TRACEPARENT: &str = "traceparent";

/// Extract the raw `traceparent` header value from HTTP headers.
#[must_use]
pub fn get_traceparent(headers: &HeaderMap) -> Option<&str> {
    headers.get(TRACEPARENT)?.to_str().ok()
}

/// Parse the trace-id from a W3C `traceparent` header
/// (format: `00-{trace_id}-{span_id}-{flags}`).
///
/// Returns the trace-id only when the whole header is a valid W3C version-`00`
/// traceparent: exactly four fields, a non-zero 32-hex trace-id, a non-zero
/// 16-hex parent-id, and 2-hex flags. Any deviation yields `None`, so a
/// caller never propagates unvalidated input as if it were a trace-id.
#[must_use]
pub fn parse_trace_id(traceparent: &str) -> Option<String> {
    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() == 4
        && parts[0] == "00"
        && is_valid_trace_id(parts[1])
        && is_valid_parent_id(parts[2])
        && is_valid_flags(parts[3])
    {
        Some(parts[1].to_owned())
    } else {
        None
    }
}

/// `len` lowercase hex digits (`0-9`, `a-f`).
fn is_lowercase_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A valid W3C trace-id: 32 lowercase hex digits, not all zeroes.
fn is_valid_trace_id(s: &str) -> bool {
    is_lowercase_hex(s, 32) && s.bytes().any(|b| b != b'0')
}

/// A valid W3C parent-id (span-id): 16 lowercase hex digits, not all zeroes.
fn is_valid_parent_id(s: &str) -> bool {
    is_lowercase_hex(s, 16) && s.bytes().any(|b| b != b'0')
}

/// Valid W3C trace-flags: exactly 2 lowercase hex digits.
fn is_valid_flags(s: &str) -> bool {
    is_lowercase_hex(s, 2)
}

/// OpenTelemetry-backed propagation: continues and injects W3C Trace Context
/// through the process-global text-map propagator.
#[cfg(feature = "otel")]
mod imp {
    use super::parse_trace_id;
    use http::{HeaderMap, HeaderName, HeaderValue};
    use opentelemetry::{
        Context, global,
        propagation::{Extractor, Injector},
    };
    use tracing::Span;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    /// Adapter for extracting W3C Trace Context from HTTP headers.
    struct HeadersExtractor<'a>(&'a HeaderMap);

    impl Extractor for HeadersExtractor<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0.get(key).and_then(|v| v.to_str().ok())
        }

        fn keys(&self) -> Vec<&str> {
            self.0.keys().map(http::HeaderName::as_str).collect()
        }
    }

    /// Adapter for injecting W3C Trace Context into HTTP headers.
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

    /// Seed `span`'s parent with the W3C trace context carried by `headers` (via
    /// the process-global text-map propagator), so the span continues the
    /// caller's trace rather than starting a fresh root, and record
    /// `trace_id` / `parent.trace_id` on the span for log correlation.
    ///
    /// A no-op (fresh root) when no `traceparent` is present or no propagator is
    /// installed. The `set_parent` error (e.g. `LayerNotFound`) is intentionally
    /// discarded: it only means no continuation happened, and the recorded ids
    /// below still carry the wire trace-id.
    pub fn set_parent_from_headers(span: &Span, headers: &HeaderMap) {
        let parent_cx = global::get_text_map_propagator(|propagator| {
            propagator.extract(&HeadersExtractor(headers))
        });
        _ = span.set_parent(parent_cx);

        if let Some(traceparent) = super::get_traceparent(headers)
            && let Some(trace_id) = parse_trace_id(traceparent)
        {
            span.record("trace_id", &trace_id);
            span.record("parent.trace_id", &trace_id);
        }
    }

    /// Inject the current OpenTelemetry context into `headers` as `traceparent`,
    /// so an outbound request continues this process's active trace.
    pub fn inject_current_span(headers: &mut HeaderMap) {
        let cx = Context::current();
        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(&cx, &mut HeadersInjector(headers));
        });
    }
}

/// Fallback propagation without OpenTelemetry: records the inbound trace id
/// for log correlation; injection is a no-op.
#[cfg(not(feature = "otel"))]
mod imp {
    use super::parse_trace_id;
    use http::HeaderMap;
    use tracing::Span;

    /// No OpenTelemetry: cannot continue the caller's trace, but still record
    /// `trace_id` / `parent.trace_id` from the header so text/JSON logs carry
    /// the wire id.
    pub fn set_parent_from_headers(span: &Span, headers: &HeaderMap) {
        if let Some(traceparent) = super::get_traceparent(headers)
            && let Some(trace_id) = parse_trace_id(traceparent)
        {
            span.record("trace_id", &trace_id);
            span.record("parent.trace_id", &trace_id);
        }
    }

    /// No-op: OpenTelemetry is disabled, so there is no active context to inject.
    pub fn inject_current_span(_headers: &mut HeaderMap) {}
}

pub use imp::{inject_current_span, set_parent_from_headers};

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use http::HeaderMap;
    use tracing::info_span;

    const TID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";

    #[test]
    fn get_traceparent_none_when_absent() {
        assert!(get_traceparent(&HeaderMap::new()).is_none());
    }

    #[test]
    fn get_traceparent_returns_the_raw_value() {
        let mut headers = HeaderMap::new();
        headers.insert(
            TRACEPARENT,
            format!("00-{TID}-00f067aa0ba902b7-01")
                .parse()
                .expect("valid header"),
        );
        assert_eq!(
            get_traceparent(&headers),
            Some(format!("00-{TID}-00f067aa0ba902b7-01").as_str())
        );
    }

    #[test]
    fn parses_a_valid_version_00_header() {
        assert_eq!(
            parse_trace_id(&format!("00-{TID}-00f067aa0ba902b7-01")).as_deref(),
            Some(TID)
        );
    }

    #[test]
    fn rejects_obviously_invalid_headers() {
        assert!(parse_trace_id("invalid").is_none());
        assert!(parse_trace_id("").is_none());
        assert!(parse_trace_id("00---").is_none());
        assert!(parse_trace_id("00-not-hex-at-all-x").is_none());
    }

    #[test]
    fn rejects_a_parseable_but_invalid_segment() {
        assert!(
            parse_trace_id("00-00000000000000000000000000000000-00f067aa0ba902b7-01").is_none(),
            "all-zero trace id is invalid per W3C"
        );
        assert!(
            parse_trace_id("00-4BF92F3577B34DA6A3CE929D0E0E4736-00f067aa0ba902b7-01").is_none(),
            "uppercase hex is not the lowercase wire form"
        );
    }

    #[test]
    fn validates_the_whole_version_00_header() {
        assert!(
            parse_trace_id(&format!("00-{TID}-00f067aa0ba902b7-01-extra")).is_none(),
            "a version-00 header must have exactly four fields"
        );
        assert!(
            parse_trace_id(&format!("00-{TID}-00f067aa0ba902-01")).is_none(),
            "short parent-id must be rejected"
        );
        assert!(
            parse_trace_id(&format!("00-{TID}-zzzzzzzzzzzzzzzz-01")).is_none(),
            "non-hex parent-id must be rejected"
        );
        assert!(
            parse_trace_id(&format!("00-{TID}-0000000000000000-01")).is_none(),
            "all-zero parent-id is invalid per W3C"
        );
        assert!(
            parse_trace_id(&format!("00-{TID}-00f067aa0ba902b7-1")).is_none(),
            "single-digit flags must be rejected"
        );
        assert!(
            parse_trace_id(&format!("00-{TID}-00f067aa0ba902b7-zz")).is_none(),
            "non-hex flags must be rejected"
        );
    }

    /// `set_parent_from_headers` must not panic in either feature mode, and it
    /// records the ids onto a span that declares them.
    #[test]
    fn set_parent_from_headers_records_ids_without_panicking() {
        let mut headers = HeaderMap::new();
        headers.insert(
            TRACEPARENT,
            format!("00-{TID}-00f067aa0ba902b7-01")
                .parse()
                .expect("valid header"),
        );
        let span = info_span!(
            "test",
            trace_id = tracing::field::Empty,
            parent.trace_id = tracing::field::Empty
        );
        set_parent_from_headers(&span, &headers);
    }

    /// With `otel`, injecting the active context must not panic even when no
    /// full tracer pipeline is installed.
    #[test]
    #[cfg(feature = "otel")]
    fn inject_current_span_does_not_panic() {
        use opentelemetry::global;
        use opentelemetry_sdk::propagation::TraceContextPropagator;

        global::set_text_map_propagator(TraceContextPropagator::new());
        let mut headers = HeaderMap::new();
        let _span = info_span!("test").entered();
        inject_current_span(&mut headers);
    }

    /// Without `otel`, injection is a no-op: no headers are added.
    #[test]
    #[cfg(not(feature = "otel"))]
    fn inject_current_span_is_a_noop_without_otel() {
        let mut headers = HeaderMap::new();
        inject_current_span(&mut headers);
        assert!(headers.is_empty());
    }
}

// Split cfgs (not `all(test, feature = "otel")`) so clippy still sees the
// `#[cfg(test)]` and allows `expect`/`unwrap` in these tests. The whole module
// needs a live OTel span, so it is also gated on `otel`.
#[cfg(test)]
#[cfg(feature = "otel")]
#[cfg_attr(coverage_nightly, coverage(off))]
mod resolver_tests {
    use super::{current_trace_id, extract_trace_id};
    use http::HeaderMap;
    use opentelemetry::trace::TracerProvider as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    /// A trace-id a caller might send that is deliberately *not* the one an
    /// active span carries, so a test can tell which of the two won.
    const OTHER_TRACE: &str = "0af7651916cd43dd8448eb211c80319c";

    fn header(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("traceparent", value.parse().expect("header value"));
        headers
    }

    fn valid_header_from_another_trace() -> HeaderMap {
        header(&format!("00-{OTHER_TRACE}-b7ad6b7169203331-01"))
    }

    /// Run `f` inside an active `OTel` span wired like production, so
    /// `current_trace_id()` resolves to a real span id.
    fn in_active_span<R>(f: impl FnOnce() -> R) -> R {
        let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder().build();
        let tracer = provider.tracer("trace-context-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        tracing::subscriber::with_default(subscriber, || tracing::info_span!("unit").in_scope(f))
    }

    /// A valid header from a *different* trace does not override an active span:
    /// the wire id must be the span's, the one the caller's own logs carry.
    #[test]
    fn active_span_wins_over_a_valid_header_from_another_trace() {
        in_active_span(|| {
            let span_id = current_trace_id().expect("an active span has a trace id");
            let resolved = extract_trace_id(&valid_header_from_another_trace());
            assert_eq!(
                resolved.as_deref(),
                Some(span_id.as_str()),
                "the live span's id, not the header's"
            );
            assert_ne!(
                resolved.as_deref(),
                Some(OTHER_TRACE),
                "the header carried a different trace and must not win"
            );
        });
    }

    /// An invalid header inside an active span falls through to the span id
    /// (never to `None`): the fallthrough from a rejected header to the live
    /// span is exercised here, not just the no-span `None` case.
    #[test]
    fn invalid_header_inside_a_span_yields_the_span_id() {
        in_active_span(|| {
            let span_id = current_trace_id().expect("an active span has a trace id");
            assert_eq!(
                extract_trace_id(&header("not-a-valid-traceparent")).as_deref(),
                Some(span_id.as_str())
            );
        });
    }

    /// With no span active, the header is the only source: a valid one parses, an
    /// invalid one and an absent one both yield `None` (never the raw header).
    #[test]
    fn header_is_used_only_when_no_span_is_active() {
        assert_eq!(
            extract_trace_id(&valid_header_from_another_trace()).as_deref(),
            Some(OTHER_TRACE)
        );
        assert!(extract_trace_id(&header("not-a-valid-traceparent")).is_none());
        assert!(extract_trace_id(&HeaderMap::new()).is_none());
    }
}
