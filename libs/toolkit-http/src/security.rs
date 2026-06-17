//! HTTP security utilities.
//!
//! `SecurityContext` propagation over HTTP uses a single header,
//! `Authorization: Bearer <jwt>`, carrying the original tenant-plane JWT. The
//! token is forwarded as-is across hops and **re-validated at every hop** —
//! there is no trusted-peer fast path (zero-trust; see ADR-0008). No binary
//! `x-secctx-bin` encoding is used over HTTP.

use http::{HeaderMap, HeaderValue, header::AUTHORIZATION};
use secrecy::ExposeSecret;
use toolkit_security::SecurityContext;

/// Maximum body preview size for error messages (8KB).
///
/// When an HTTP request returns a non-2xx status, the response body is included
/// in the error message for debugging. This constant limits how much of the body
/// is read to prevent memory issues with large error responses.
pub const ERROR_BODY_PREVIEW_LIMIT: usize = 8 * 1024;

/// Errors raised while extracting a bearer token from HTTP request headers.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum SecCtxHttpError {
    /// No `Authorization` header was present on the request.
    #[error("missing Authorization header")]
    MissingAuthHeader,
    /// The `Authorization` header was present but not a valid `Bearer` token
    /// (non-ASCII bytes, wrong scheme, or no scheme/token separator).
    #[error("invalid Authorization header format")]
    InvalidAuthHeader,
    /// The `Authorization` header used the `Bearer` scheme but the token was
    /// empty after trimming surrounding whitespace.
    #[error("empty bearer token")]
    EmptyToken,
}

/// Attach the tenant-plane JWT from `secctx` to an outgoing request as
/// `Authorization: Bearer <jwt>`.
///
/// The secret is exposed only at this transport boundary and the resulting
/// header value is marked sensitive so it is never logged. If `secctx` carries
/// no bearer token, or the token cannot be represented as a header value, the
/// request is left unchanged.
pub fn attach_bearer_http<B>(request: &mut http::Request<B>, secctx: &SecurityContext) {
    let Some(token) = secctx.bearer_token() else {
        return;
    };
    // Expose the secret only here, at the transport boundary, and never log it.
    let Ok(mut value) = HeaderValue::from_str(&format!("Bearer {}", token.expose_secret())) else {
        return;
    };
    value.set_sensitive(true);
    request.headers_mut().insert(AUTHORIZATION, value);
}

/// Extract the raw bearer token from the `Authorization` header.
///
/// The `Bearer` scheme is matched case-insensitively, surrounding whitespace is
/// trimmed, and an empty token is rejected.
///
/// # Errors
///
/// Returns [`SecCtxHttpError`] when the header is absent, not a valid `Bearer`
/// credential, or carries an empty token.
pub fn extract_bearer_http(headers: &HeaderMap) -> Result<String, SecCtxHttpError> {
    let header = headers
        .get(AUTHORIZATION)
        .ok_or(SecCtxHttpError::MissingAuthHeader)?;
    let raw = header
        .to_str()
        .map_err(|_| SecCtxHttpError::InvalidAuthHeader)?;

    let (scheme, token) = raw
        .trim_start()
        .split_once(char::is_whitespace)
        .ok_or(SecCtxHttpError::InvalidAuthHeader)?;

    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(SecCtxHttpError::InvalidAuthHeader);
    }

    let token = token.trim();
    if token.is_empty() {
        return Err(SecCtxHttpError::EmptyToken);
    }

    Ok(token.to_owned())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn secctx_with_token(token: &str) -> SecurityContext {
        SecurityContext::builder()
            .subject_id(uuid::Uuid::nil())
            .subject_tenant_id(uuid::Uuid::nil())
            .bearer_token(token.to_owned())
            .build()
            .expect("valid security context")
    }

    #[test]
    fn attach_extract_round_trip() {
        let secctx = secctx_with_token("header.payload.signature");
        let mut request = http::Request::new(());
        attach_bearer_http(&mut request, &secctx);

        let token = extract_bearer_http(request.headers()).expect("token extracted");
        assert_eq!(token, "header.payload.signature");
    }

    #[test]
    fn attach_marks_header_sensitive() {
        let secctx = secctx_with_token("abc.def.ghi");
        let mut request = http::Request::new(());
        attach_bearer_http(&mut request, &secctx);

        let value = request.headers().get(AUTHORIZATION).expect("header set");
        assert!(value.is_sensitive());
    }

    #[test]
    fn attach_noop_when_no_token() {
        let secctx = SecurityContext::anonymous();
        let mut request = http::Request::new(());
        attach_bearer_http(&mut request, &secctx);

        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    #[test]
    fn extract_missing_header() {
        let headers = HeaderMap::new();
        assert_eq!(
            extract_bearer_http(&headers),
            Err(SecCtxHttpError::MissingAuthHeader)
        );
    }

    #[test]
    fn extract_case_insensitive_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("bEaReR my-token"));
        assert_eq!(extract_bearer_http(&headers).unwrap(), "my-token");
    }

    #[test]
    fn extract_trims_surrounding_whitespace() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("  Bearer   my-token  "));
        assert_eq!(extract_bearer_http(&headers).unwrap(), "my-token");
    }

    #[test]
    fn extract_wrong_scheme() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Basic dXNlcjpwYXNz"));
        assert_eq!(
            extract_bearer_http(&headers),
            Err(SecCtxHttpError::InvalidAuthHeader)
        );
    }

    #[test]
    fn extract_no_separator() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer"));
        assert_eq!(
            extract_bearer_http(&headers),
            Err(SecCtxHttpError::InvalidAuthHeader)
        );
    }

    #[test]
    fn extract_empty_token() {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static("Bearer    "));
        assert_eq!(
            extract_bearer_http(&headers),
            Err(SecCtxHttpError::EmptyToken)
        );
    }
}
