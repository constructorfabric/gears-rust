//! A weak `ETag` over the served JSON body, and `304` on a matching `If-None-Match`.
//!
//! The tag is not a row version. Single-resource reads keep their strong `ETag`
//! for `If-Match`. A list body differs per caller, so this tag does too.

use aws_lc_rs::digest::{SHA256, digest};
use axum::body::Body;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use http::{HeaderMap, HeaderValue, StatusCode, header};
use toolkit_canonical_errors::CanonicalError;

/// `Cache-Control` for one conditional read.
#[derive(Clone, Copy, Debug)]
pub struct CacheHeaders {
    /// The header value, for example `private, no-cache`.
    pub cache_control: &'static str,
}

/// The browser stores the answer and must revalidate it.
pub const PRIVATE_REVALIDATE: CacheHeaders = CacheHeaders {
    cache_control: "private, no-cache",
};

/// Characters of base64url kept from the SHA-256 digest.
const TAG_CHARS: usize = 22;

/// The weak tag over `body`: `W/"<22 b64url chars of sha256>"`.
#[must_use]
pub fn weak_etag(body: &[u8]) -> HeaderValue {
    let digested = digest(&SHA256, body);
    let encoded = URL_SAFE_NO_PAD.encode(digested.as_ref());
    let short = encoded.get(..TAG_CHARS).unwrap_or(encoded.as_str());
    ascii_header(&format!("W/\"{short}\""))
}

/// `200` with the JSON body, `ETag` and `Cache-Control`, or `304` when `request`
/// carries a matching `If-None-Match`.
///
/// Only this `200` becomes a `304`. A value that cannot be serialized is the
/// canonical `500` `Problem` the doors declare, is never conditional, and is
/// logged once with the value's type: no body and no error text.
#[must_use]
pub fn respond<T: serde::Serialize>(
    request: &HeaderMap,
    value: &T,
    cache: CacheHeaders,
) -> Response {
    let body = match serde_json::to_vec(value) {
        Ok(body) => body,
        Err(error) => {
            let response_type = std::any::type_name::<T>();
            tracing::error!(
                response_type,
                category = ?error.classify(),
                "conditional GET: the response did not serialize"
            );
            return CanonicalError::internal(format!(
                "conditional GET: {response_type} did not serialize"
            ))
            .create()
            .into_response();
        }
    };
    let etag = weak_etag(&body);
    let matched = request
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .any(|candidate| matches_if_none_match(Some(candidate), &etag));
    if matched {
        return tagged(StatusCode::NOT_MODIFIED, etag, cache, Body::empty(), false);
    }
    tagged(StatusCode::OK, etag, cache, Body::from(body), true)
}

/// RFC 9110 weak comparison of `tag` against a comma list or `*`.
///
/// A missing header does not match. `*` matches. A strong tag matches the weak
/// tag with the same opaque value, in any list position.
#[must_use]
pub fn matches_if_none_match(header: Option<&HeaderValue>, tag: &HeaderValue) -> bool {
    let Some(raw) = header.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let wanted = tag.to_str().ok().and_then(opaque_tag);
    list_elements(raw).into_iter().any(|element| {
        let element = element.trim();
        element == "*" || wanted.is_some_and(|opaque| opaque_tag(element) == Some(opaque))
    })
}

fn tagged(
    status: StatusCode,
    etag: HeaderValue,
    cache: CacheHeaders,
    body: Body,
    json: bool,
) -> Response {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(header::ETAG, etag);
    headers.insert(header::CACHE_CONTROL, ascii_header(cache.cache_control));
    if json {
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
    }
    response
}

fn ascii_header(value: &str) -> HeaderValue {
    HeaderValue::from_str(value).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// The opaque part of one entity-tag, `W/` dropped. `None` when `raw` is not an entity-tag.
fn opaque_tag(raw: &str) -> Option<&str> {
    let raw = raw.trim().strip_prefix("W/").unwrap_or(raw.trim());
    let inner = raw.strip_prefix('"')?.strip_suffix('"')?;
    inner
        .bytes()
        .all(|byte| byte == b'!' || (b'#'..=b'~').contains(&byte) || byte >= 0x80)
        .then_some(inner)
}

/// Split a list on the commas outside quotes.
fn list_elements(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (index, byte) in value.bytes().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                parts.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

#[cfg(test)]
#[path = "conditional_get_tests.rs"]
mod conditional_get_tests;
