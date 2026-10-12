// Created: 2026-08-13 by Virtuozzo International GmbH
//! REST surface.

/// The `If-Match` tag a mutation presents, read the same way on every
/// mutation handler of this service.
///
/// Only the header's HTTP framing comes off — surrounding whitespace and one
/// pair of double quotes, which an RFC 7232 client puts around the validator
/// it was given — and nothing else: a weak validator (`W/"…"`) is not
/// stripped and so never matches, as `If-Match`'s strong comparison asks.
/// What is left goes to the domain's precondition evaluator, which compares
/// it verbatim. Absent is `None`, and only the evaluator decides what an
/// absent tag means for the operation at hand.
pub(crate) fn if_match(headers: &axum::http::HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().trim_matches('"'))
}

/// A state tag as the `ETag` header carries it: a strong entity tag, the
/// tag in double quotes (RFC 9110 §8.8.3).
///
/// Only the header is framed. A body's `etag` field is the bare tag, and
/// [`if_match`] takes either back — the quotes are HTTP's, not the tag's — so
/// a client may echo the header or copy the field.
pub(crate) fn etag_header(tag: &str) -> String {
    format!("\"{tag}\"")
}

/// The id an audit record is correlated by: the platform's per-request
/// `x-request-id`. The api-gateway sets it on every inbound request, writes it
/// to the access log, and returns it to the caller, so an audit row joins to
/// the access-log line and to the id the caller saw.
///
/// Deliberately **not** the W3C `trace_id` (`toolkit::api::extract_trace_id`):
/// that names a whole distributed trace, lives in the Problem envelope / log
/// lines, and is absent from a success response and without `OTel`. When no
/// `x-request-id` is present — a gear reached directly, not through the gateway
/// — a fresh UUID is minted, as the contribution reconciler does when there is
/// no request to take one from.
pub(crate) fn audit_request_id(headers: &axum::http::HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned)
}

pub mod access_dto;
pub mod access_handlers;
pub mod access_routes;
pub mod declaration_dto;
pub mod declaration_handlers;
pub mod declaration_routes;
pub mod dto;
pub mod handlers;
pub mod routes;
pub mod search_dto;
pub mod search_handlers;
pub mod search_routes;
pub mod setting_dto;
pub mod setting_filter;
pub mod setting_handlers;
pub mod setting_routes;
pub mod value_dto;
pub mod value_handlers;
pub mod value_routes;

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod audit_request_id_tests {
    use super::audit_request_id;
    use axum::http::{HeaderMap, HeaderName, HeaderValue};
    use uuid::Uuid;

    fn with(name: &str, value: &str) -> HeaderMap {
        let mut map = HeaderMap::new();
        map.insert(
            HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
            HeaderValue::from_str(value).expect("a header value"),
        );
        map
    }

    /// The gateway's `x-request-id` is lifted verbatim. This is the one
    /// derivation every mutation handler (value, category, access) uses, so it
    /// is pinned once here rather than at each site.
    #[test]
    fn the_x_request_id_is_the_audit_request_id() {
        assert_eq!(
            audit_request_id(&with("x-request-id", "req-abc123")),
            "req-abc123"
        );
    }

    /// A W3C `traceparent` is NOT consumed: the trace id is a different
    /// identifier (it belongs in the Problem envelope), so with only a
    /// `traceparent` present the id is minted, not lifted from the trace.
    #[test]
    fn a_traceparent_is_not_consumed_as_the_request_id() {
        let traceparent = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        let minted = audit_request_id(&with("traceparent", traceparent));
        assert!(Uuid::parse_str(&minted).is_ok(), "{minted}");
        assert!(
            !minted.contains("0af7651916cd43dd8448eb211c80319c"),
            "the trace id must not leak into request_id: {minted}"
        );
    }

    /// Reached directly, without the gateway: a fresh UUID is minted, and two
    /// requests never share one.
    #[test]
    fn absent_x_request_id_mints_a_fresh_uuid() {
        let first = audit_request_id(&HeaderMap::new());
        assert!(Uuid::parse_str(&first).is_ok(), "{first}");
        assert_ne!(
            first,
            audit_request_id(&HeaderMap::new()),
            "two requests are not one"
        );
    }
}

#[cfg(test)]
#[path = "if_match_tests.rs"]
mod if_match_tests;

#[cfg(test)]
#[path = "read_surface_tests.rs"]
mod read_surface_tests;

#[cfg(test)]
#[path = "category_surface_tests.rs"]
mod category_surface_tests;

#[cfg(test)]
#[path = "access_surface_tests.rs"]
mod access_surface_tests;

#[cfg(test)]
#[path = "declaration_surface_tests.rs"]
mod declaration_surface_tests;

#[cfg(test)]
#[path = "value_surface_tests.rs"]
mod value_surface_tests;
