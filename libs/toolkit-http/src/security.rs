//! HTTP security utilities.
//!
//! `SecurityContext` propagation over HTTP uses a single header,
//! `Authorization: Bearer <jwt>`, carrying the original tenant-plane JWT. The
//! token is forwarded as-is across hops and **re-validated at every hop** —
//! there is no trusted-peer fast path (zero-trust). No binary
//! `x-secctx-bin` encoding is used over HTTP.

use http::{HeaderValue, header::AUTHORIZATION};
use secrecy::{ExposeSecret, SecretString};
use toolkit_security::SecurityContext;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;

/// Maximum body preview size for error messages (8KB).
///
/// When an HTTP request returns a non-2xx status, the response body is included
/// in the error message for debugging. This constant limits how much of the body
/// is read to prevent memory issues with large error responses.
pub const ERROR_BODY_PREVIEW_LIMIT: usize = 8 * 1024;

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
        tracing::warn!(
            "bearer token contains invalid HTTP header characters; outgoing request sent without Authorization header"
        );
        return;
    };
    value.set_sensitive(true);
    request.headers_mut().insert(AUTHORIZATION, value);
}

/// Attach a platform-plane internal `token` to an outgoing request as the
/// `X-ToolKit-Internal-Token` header.
///
/// The token is carried raw (no `Bearer` scheme) and **never** on
/// `Authorization`, to avoid colliding with the tenant-plane user JWT.
/// The secret is exposed only at this transport boundary
/// and the resulting header value is marked sensitive so it is never logged. If
/// the token cannot be represented as a header value, the request is left
/// unchanged.
pub fn attach_internal_token_http<B>(request: &mut http::Request<B>, token: &SecretString) {
    // Expose the secret only here, at the transport boundary, and never log it.
    let Ok(mut value) = HeaderValue::from_str(token.expose_secret()) else {
        tracing::warn!(
            "internal token contains invalid HTTP header characters; outgoing request sent without X-ToolKit-Internal-Token header"
        );
        return;
    };
    value.set_sensitive(true);
    request.headers_mut().insert(INTERNAL_TOKEN_HEADER, value);
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
    fn attach_sets_bearer_header() {
        let secctx = secctx_with_token("header.payload.signature");
        let mut request = http::Request::new(());
        attach_bearer_http(&mut request, &secctx);

        let value = request.headers().get(AUTHORIZATION).expect("header set");
        assert_eq!(value.to_str().unwrap(), "Bearer header.payload.signature");
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
    fn internal_token_uses_dedicated_header_not_authorization() {
        let mut request = http::Request::new(());
        attach_internal_token_http(&mut request, &SecretString::from("sa.jwt.token"));

        assert!(request.headers().get(INTERNAL_TOKEN_HEADER).is_some());
        assert!(request.headers().get(AUTHORIZATION).is_none());
    }

    #[test]
    fn internal_token_header_is_sensitive() {
        let mut request = http::Request::new(());
        attach_internal_token_http(&mut request, &SecretString::from("sa.jwt.token"));

        let value = request
            .headers()
            .get(INTERNAL_TOKEN_HEADER)
            .expect("header set");
        assert!(value.is_sensitive());
    }
}
