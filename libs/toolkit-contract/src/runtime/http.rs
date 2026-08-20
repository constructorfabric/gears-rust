//! HTTP helpers shared by the generated REST client codegen.
//!
//! These are intentionally low-level and provider-agnostic so that the macro
//! output stays small and the helpers can be unit-tested in isolation.

use bytes::Bytes;
use futures_core::Stream;
use http_body::Body;
use http_body_util::BodyStream;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use toolkit_canonical_errors::Problem;
use toolkit_http::RequestBuilder;

use crate::ir::binding::{HttpFieldBinding, HttpMethod, HttpMethodBindingIr};
use crate::runtime::config::InternalTokenProvider;
use crate::runtime::transport_error::TransportError;

// RFC 3986 path-segment encode set: encode everything except unreserved
// characters (`A-Z a-z 0-9 - . _ ~`).
const PATH_SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'\'')
    .add(b'(')
    .add(b')')
    .add(b'*')
    .add(b'+')
    .add(b',')
    .add(b'/')
    .add(b':')
    .add(b';')
    .add(b'<')
    .add(b'=')
    .add(b'>')
    .add(b'?')
    .add(b'@')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// Adapt any `http_body::Body` into a `Stream<Item = Result<Bytes, E>>` of
/// data frames, dropping trailers.
///
/// `toolkit_http::HttpResponse::into_body()` returns a `ResponseBody` that
/// implements [`http_body::Body`] but the SSE parser
/// ([`crate::runtime::sse::parse_sse_stream_with_id`]) expects a flat
/// `Stream` of byte chunks. SSE has no use for trailers, so non-data frames
/// are simply skipped.
pub fn body_to_byte_stream<B>(body: B) -> impl Stream<Item = Result<Bytes, B::Error>> + Send
where
    B: Body<Data = Bytes> + Send + 'static,
    B::Error: Send + 'static,
{
    use futures_util::StreamExt as _;
    BodyStream::new(body).filter_map(|frame_res| async move {
        match frame_res {
            Ok(frame) => frame.into_data().ok().map(Ok),
            Err(e) => Some(Err(e)),
        }
    })
}

/// Build a fully-qualified URL by substituting path parameters and appending a
/// pre-encoded query string, returning [`TransportError`] on failure.
///
/// `fields` is expected to be a JSON object whose keys correspond to the
/// `field` names referenced by `method_binding.field_bindings`. Missing path
/// parameters yield [`TransportError::UrlBuild`].
///
/// `query` is the already-encoded query string (no leading `?`), produced by
/// [`crate::query::to_query_string`]. It arrives pre-encoded rather than as
/// structured data on purpose: the server decodes it with the same
/// `serde_html_form` codec, and routing it through this function's own
/// serializer is what previously let the two ends disagree on `Vec` and nested
/// shapes.
///
/// # Errors
/// Returns [`TransportError::UrlBuild`] when a required path parameter is missing,
/// null, or empty, or when a referenced field is not convertible to a string.
pub fn build_request_url(
    base_url: &str,
    base_path: &str,
    method_binding: &HttpMethodBindingIr,
    fields: &serde_json::Value,
    query: Option<&str>,
) -> Result<String, TransportError> {
    let mut path = method_binding.path_template.clone();

    for binding in &method_binding.field_bindings {
        match binding {
            HttpFieldBinding::Path { field, param } => {
                let value = field_as_string(fields, field)?.ok_or_else(|| {
                    TransportError::UrlBuild(format!(
                        "required path parameter '{field}' is missing or null"
                    ))
                })?;
                if value.is_empty() {
                    return Err(TransportError::UrlBuild(format!(
                        "required path parameter '{field}' is empty"
                    )));
                }
                let encoded = utf8_percent_encode(&value, PATH_SEGMENT).to_string();
                path = path.replace(&format!("{{{param}}}"), &encoded);
            }
            // Query values are encoded by the caller; the binding entry stays in
            // the IR for validation and spec generation.
            HttpFieldBinding::Query { .. } | HttpFieldBinding::Body => {}
        }
    }

    let base = base_url.trim_end_matches('/');
    let base_p = base_path.trim_end_matches('/');
    let mut url = format!("{base}{base_p}{path}");

    if let Some(q) = query.filter(|q| !q.is_empty()) {
        url.push('?');
        url.push_str(q);
    }

    Ok(url)
}

/// Attach the platform-plane credential from `provider` (if any) to a REST
/// [`RequestBuilder`] as the sensitive `X-ToolKit-Internal-Token` header.
///
/// The single audited REST emit point, shared by the unary-attempt closure and
/// the SSE reconnect factory (so both re-resolve per attempt and pick up
/// rotation). REST sibling of [`crate::grpc::attach_internal_token`]; both
/// delegate the attach policy to [`InternalTokenProvider::resolve_for_attach`].
pub fn attach_internal_token(
    builder: RequestBuilder,
    provider: Option<&InternalTokenProvider>,
    rpc: &str,
) -> RequestBuilder {
    match InternalTokenProvider::resolve_for_attach(provider, rpc) {
        Some(token) => builder.internal_token_auth(&token),
        None => builder,
    }
}

/// Map an HTTP method enum to [`http::Method`].
#[must_use]
pub fn to_http_method(method: HttpMethod) -> http::Method {
    match method {
        HttpMethod::Get => http::Method::GET,
        HttpMethod::Post => http::Method::POST,
        HttpMethod::Put => http::Method::PUT,
        HttpMethod::Patch => http::Method::PATCH,
        HttpMethod::Delete => http::Method::DELETE,
    }
}

/// Map a non-success HTTP response into a [`TransportError`].
///
/// Tries to parse the body as an RFC 9457 [`Problem`] envelope first,
/// falling back to [`TransportError::HttpStatus`] with a truncated body
/// excerpt for peers that don't speak the canonical-errors envelope.
///
/// `retry_after` is the parsed `Retry-After` header (see [`parse_retry_after`]);
/// it is attached to both the [`TransportError::Problem`] and the
/// [`TransportError::HttpStatus`] fallback so the retry loop honors a
/// server-advised backoff regardless of whether the peer speaks canonical
/// errors.
#[must_use]
pub fn map_http_error(
    status: u16,
    body: String,
    retry_after: Option<std::time::Duration>,
) -> TransportError {
    if let Ok(problem) = serde_json::from_str::<Problem>(&body) {
        return TransportError::Problem {
            problem: Box::new(problem),
            retry_after,
        };
    }
    TransportError::HttpStatus {
        status,
        body: truncate(body, 256),
        retry_after,
    }
}

/// Parse a `Retry-After` response header as a delta-seconds [`Duration`].
///
/// Only the numeric delta-seconds form is supported (the common case for
/// `429`/`503`); the HTTP-date form is ignored (returns `None`), as is a
/// missing or malformed header.
#[must_use]
pub fn parse_retry_after(headers: &http::HeaderMap) -> Option<std::time::Duration> {
    let raw = headers.get(http::header::RETRY_AFTER)?;
    let secs: u64 = raw.to_str().ok()?.trim().parse().ok()?;
    Some(std::time::Duration::from_secs(secs))
}

fn field_as_string(
    fields: &serde_json::Value,
    field_name: &str,
) -> Result<Option<String>, TransportError> {
    let Some(value) = fields.get(field_name) else {
        return Ok(None);
    };
    match value {
        serde_json::Value::String(s) => Ok(Some(s.clone())),
        serde_json::Value::Number(n) => Ok(Some(n.to_string())),
        serde_json::Value::Bool(b) => Ok(Some(b.to_string())),
        serde_json::Value::Null => Ok(None),
        _ => Err(TransportError::UrlBuild(format!(
            "field '{field_name}' has non-scalar type and cannot be embedded into the URL"
        ))),
    }
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        // `s` is a peer-controlled response body (arbitrary UTF-8); truncating
        // at a raw byte offset panics if `max` lands inside a multi-byte
        // character. Floor to the nearest char boundary at or below `max`.
        let cut = (0..=max)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        s.truncate(cut);
        s.push('\u{2026}');
    }
    s
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::ir::binding::{HttpFieldBinding, HttpMethodBindingIr};

    fn binding(template: &str, fields: Vec<HttpFieldBinding>) -> HttpMethodBindingIr {
        HttpMethodBindingIr {
            method_name: "x".to_owned(),
            http_method: HttpMethod::Get,
            path_template: template.to_owned(),
            field_bindings: fields,
            retryable: false,
            streaming: false,
            optional: false,
        }
    }

    #[test]
    fn substitutes_path_param() {
        let b = binding(
            "/items/{id}",
            vec![HttpFieldBinding::Path {
                field: "id".into(),
                param: "id".into(),
            }],
        );
        let url = build_request_url(
            "https://x.example",
            "/api",
            &b,
            &serde_json::json!({ "id": "42" }),
            None,
        )
        .unwrap();
        assert_eq!(url, "https://x.example/api/items/42");
    }

    #[test]
    fn appends_the_encoded_query_string() {
        // The query arrives already encoded (by `crate::query::to_query_string`,
        // the same codec the server decodes with); this function only joins it
        // onto the URL.
        let b = binding(
            "/list",
            vec![HttpFieldBinding::Query {
                field: "filter".into(),
                param: "filter".into(),
            }],
        );
        let url = build_request_url(
            "https://x.example",
            "/api",
            &b,
            &serde_json::json!({}),
            Some("status=paid&currency=USD"),
        )
        .unwrap();
        assert_eq!(url, "https://x.example/api/list?status=paid&currency=USD");
    }

    #[test]
    fn omits_the_separator_for_an_empty_query() {
        let b = binding("/list", vec![]);
        for query in [None, Some("")] {
            let url = build_request_url(
                "https://x.example",
                "/api",
                &b,
                &serde_json::json!({}),
                query,
            )
            .unwrap();
            assert_eq!(url, "https://x.example/api/list", "query = {query:?}");
        }
    }

    #[test]
    fn maps_problem_envelope() {
        // Canonical RFC 9457 Problem (per docs/arch/errors/DESIGN.md §3.3).
        let body = serde_json::json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.internal.v1~",
            "title": "Internal",
            "status": 500,
            "detail": "broke",
            "context": {}
        })
        .to_string();
        let err = map_http_error(500, body, None);
        match err {
            TransportError::Problem { problem: p, .. } => {
                assert_eq!(p.status, 500);
                assert_eq!(p.detail, "broke");
                assert!(p.problem_type.contains("internal"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn problem_envelope_carries_retry_after() {
        // A canonical `Problem` peer that also sends `Retry-After` must have the
        // advised delay threaded onto the `Problem` variant so the retry loop
        // honors it (M-16) — not just the non-canonical `HttpStatus` fallback.
        let body = serde_json::json!({
            "type": "gts://gts.cf.core.errors.err.v1~cf.core.err.service_unavailable.v1~",
            "title": "Service unavailable",
            "status": 503,
            "detail": "draining",
            "context": {}
        })
        .to_string();
        let err = map_http_error(503, body, Some(std::time::Duration::from_secs(2)));
        assert_eq!(err.retry_after(), Some(std::time::Duration::from_secs(2)));
        assert!(matches!(err, TransportError::Problem { .. }));
    }

    #[test]
    fn truncate_does_not_panic_on_multibyte_char_at_boundary() {
        // 255 ASCII bytes + a 3-byte UTF-8 char (é is 2 bytes; use a 3-byte
        // char to straddle byte 256 exactly) — a raw `s.truncate(256)` would
        // panic because byte 256 falls inside the multi-byte character.
        let body = format!("{}€", "a".repeat(255)); // '€' is 3 bytes (U+20AC)
        assert_eq!(body.len(), 258);
        let out = truncate(body, 256);
        // Truncated to the last valid boundary at or below 256 (255, since the
        // '€' starts at byte 255), with the ellipsis marker appended.
        assert_eq!(out, format!("{}\u{2026}", "a".repeat(255)));
    }

    #[test]
    fn falls_back_to_http_status_for_non_problem_body() {
        let err = map_http_error(503, "service unavailable".into(), None);
        match err {
            TransportError::HttpStatus { status, body, .. } => {
                assert_eq!(status, 503);
                assert!(body.contains("service unavailable"));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_retry_after_delta_seconds() {
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::RETRY_AFTER, "2".parse().unwrap());
        assert_eq!(
            parse_retry_after(&headers),
            Some(std::time::Duration::from_secs(2))
        );

        // HTTP-date form is ignored (unsupported), as is a missing header.
        let mut date = http::HeaderMap::new();
        date.insert(
            http::header::RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(parse_retry_after(&date), None);
        assert_eq!(parse_retry_after(&http::HeaderMap::new()), None);
    }

    #[test]
    fn missing_path_param_returns_url_build_error() {
        let b = binding(
            "/items/{id}",
            vec![HttpFieldBinding::Path {
                field: "id".into(),
                param: "id".into(),
            }],
        );
        let err = build_request_url(
            "https://x.example",
            "/api",
            &b,
            &serde_json::json!({}),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, TransportError::UrlBuild(_)));
    }
}
