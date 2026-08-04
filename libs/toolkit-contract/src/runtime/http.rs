//! HTTP helpers shared by the generated REST client codegen.
//!
//! These are intentionally low-level and provider-agnostic so that the macro
//! output stays small and the helpers can be unit-tested in isolation.

use bytes::Bytes;
use futures_core::Stream;
use http_body::Body;
use http_body_util::BodyStream;
use toolkit_canonical_errors::Problem;
use percent_encoding::{AsciiSet, CONTROLS, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::ir::binding::{HttpFieldBinding, HttpMethod, HttpMethodBindingIr};
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

/// Build a fully-qualified URL by substituting path parameters and appending
/// query parameters. Mirrors [`crate::http::dispatch::build_url`] but emits
/// [`TransportError`] instead of the historical `ContractError` type.
///
/// `fields` is expected to be a JSON object whose keys correspond to the
/// `field` names referenced by `method_binding.field_bindings`. Missing path
/// parameters yield [`TransportError::UrlBuild`].
///
/// # Errors
/// Returns [`TransportError::UrlBuild`] when a required path parameter is missing,
/// null, or empty, or when a referenced field is not convertible to a string.
pub fn build_request_url(
    base_url: &str,
    base_path: &str,
    method_binding: &HttpMethodBindingIr,
    fields: &serde_json::Value,
) -> Result<String, TransportError> {
    let mut path = method_binding.path_template.clone();
    let mut query_pairs: Vec<(String, String)> = Vec::new();

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
            HttpFieldBinding::Query { field, param } => match fields.get(field) {
                Some(value) if !value.is_null() => {
                    flatten_query_value(param, value, &mut query_pairs);
                }
                _ => {}
            },
            HttpFieldBinding::Body | HttpFieldBinding::Header { .. } => {}
        }
    }

    let base = base_url.trim_end_matches('/');
    let base_p = base_path.trim_end_matches('/');
    let mut url = format!("{base}{base_p}{path}");

    if !query_pairs.is_empty() {
        url.push('?');
        for (i, (key, value)) in query_pairs.iter().enumerate() {
            if i > 0 {
                url.push('&');
            }
            // Encode BOTH key and value: object/map-typed query params derive
            // their keys from runtime JSON, so an unencoded `&`/`=` in a key
            // would smuggle extra query parameters.
            url.push_str(&urlencoded(key));
            url.push('=');
            url.push_str(&urlencoded(value));
        }
    }

    Ok(url)
}

/// Map an HTTP method enum to [`http::Method`].
#[must_use]
pub fn to_http_method(method: HttpMethod) -> http::Method {
    match method {
        HttpMethod::Get => http::Method::GET,
        HttpMethod::Post => http::Method::POST,
        HttpMethod::Put => http::Method::PUT,
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
/// it is attached to the [`TransportError::HttpStatus`] fallback so the retry
/// loop can honor it. (Canonical `Problem` responses do not carry it here.)
#[must_use]
pub fn map_http_error(
    status: u16,
    body: String,
    retry_after: Option<std::time::Duration>,
) -> TransportError {
    if let Ok(problem) = serde_json::from_str::<Problem>(&body) {
        return TransportError::Problem(problem);
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

fn flatten_query_value(param: &str, value: &serde_json::Value, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Null => {}
        serde_json::Value::String(s) => out.push((param.to_owned(), s.clone())),
        serde_json::Value::Number(n) => out.push((param.to_owned(), n.to_string())),
        serde_json::Value::Bool(b) => out.push((param.to_owned(), b.to_string())),
        serde_json::Value::Array(items) => {
            for item in items {
                flatten_query_value(param, item, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if !v.is_null() {
                    flatten_query_value(k, v, out);
                }
            }
        }
    }
}

fn urlencoded(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

fn truncate(mut s: String, max: usize) -> String {
    if s.len() > max {
        s.truncate(max);
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
        )
        .unwrap();
        assert_eq!(url, "https://x.example/api/items/42");
    }

    #[test]
    fn flattens_struct_into_query() {
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
            &serde_json::json!({ "filter": { "status": "paid", "currency": "USD" } }),
        )
        .unwrap();
        assert!(url.starts_with("https://x.example/api/list?"));
        assert!(url.contains("status=paid"));
        assert!(url.contains("currency=USD"));
    }

    #[test]
    fn encodes_query_parameter_names() {
        // An object-typed query field whose inner key contains `&`/`=` must be
        // percent-encoded so it cannot smuggle extra query parameters.
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
            &serde_json::json!({ "filter": { "a=1&b": "x" } }),
        )
        .unwrap();
        assert!(url.contains("a%3D1%26b=x"), "unexpected url: {url}");
        assert!(
            !url.contains("a=1&b=x"),
            "raw key must not smuggle params: {url}"
        );
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
            TransportError::Problem(p) => {
                assert_eq!(p.status, 500);
                assert_eq!(p.detail, "broke");
                assert!(p.problem_type.contains("internal"));
            }
            other => panic!("unexpected {other:?}"),
        }
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
        let err =
            build_request_url("https://x.example", "/api", &b, &serde_json::json!({})).unwrap_err();
        assert!(matches!(err, TransportError::UrlBuild(_)));
    }
}
