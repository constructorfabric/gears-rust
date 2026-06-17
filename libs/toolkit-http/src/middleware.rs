//! Axum middleware for tenant-plane `SecurityContext` propagation.
//!
//! Gated behind the `axum-middleware` feature. The middleware extracts the
//! bearer token from the incoming `Authorization` header and **always**
//! re-validates it via an injected [`BearerAuthenticator`] — there is no
//! trusted-peer fast path (zero-trust; see ADR-0008). On success the
//! reconstructed [`SecurityContext`] is inserted into the request extensions
//! for downstream handlers and the `AuthZ` resolver.
//!
//! Whether a route requires a user context is decided per-route by the gear's
//! REST registration (`OperationSpec.is_public`), surfaced to the middleware as
//! a [`PublicRoute`] request extension. The concrete `AuthNResolverClient`
//! adapter is injected at the gear/bootstrap layer via Axum state (OoP-4).

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{AuthnError, BearerAuthenticator};

use crate::security::{SecCtxHttpError, extract_bearer_http};

/// Retry hint (seconds) advertised when the authentication backend is
/// temporarily unavailable, mirroring `api-gateway`'s authn middleware.
const AUTHN_RETRY_AFTER_SECONDS: u64 = 5;

/// Public detail for an unexpected authentication-infrastructure failure,
/// mirroring `api-gateway`'s authn middleware. Carries no diagnostic specifics.
const AUTHN_INFRA_FAILURE_DETAIL: &str = "authentication infrastructure failure";

/// Per-route marker indicating the route does **not** require a user context.
///
/// Inserted as a route-level request extension by the gear/bootstrap layer
/// (OoP-4) for routes registered as `.public()` (i.e. `OperationSpec.is_public`
/// is `true`). When present, [`secctx_middleware`] lets a request without an
/// `Authorization` header pass through instead of returning `401`.
#[derive(Clone, Copy, Debug)]
pub struct PublicRoute;

/// Tenant-plane `SecurityContext` middleware.
///
/// Behaviour:
/// - A bearer token, if present, is **always** re-validated via the injected
///   [`BearerAuthenticator`]; on success the [`SecurityContext`] is inserted
///   into request extensions.
/// - A protected route (no [`PublicRoute`] marker) with a missing or invalid
///   `Authorization` header is rejected with `401`.
/// - A public / system-only route (carrying the [`PublicRoute`] marker) with no
///   `Authorization` header passes through.
/// - A rejected token is `401`; an unreachable backend is `503`; any other
///   unexpected authentication failure is `500`.
///
/// Rejections are rendered as canonical RFC 9457 `application/problem+json`
/// responses (via [`CanonicalError`]) so they match the platform-wide error
/// contract; `instance` / `trace_id` enrichment is left to the outer canonical
/// error middleware installed at the gear/bootstrap layer.
///
/// The handler is generic over `A`; the concrete authenticator is supplied via
/// Axum state as `Arc<A>` at the gear/bootstrap layer.
pub async fn secctx_middleware<A>(
    State(authenticator): State<Arc<A>>,
    mut request: Request,
    next: Next,
) -> Response
where
    A: BearerAuthenticator + 'static,
{
    let is_public = request.extensions().get::<PublicRoute>().is_some();

    match extract_bearer_http(request.headers()) {
        Ok(token) => match authenticator.authenticate(&token).await {
            Ok(secctx) => {
                request.extensions_mut().insert(secctx);
                next.run(request).await
            }
            Err(err) => authn_error_to_response(&err),
        },
        // No credential presented: allow through only for public/system-only
        // routes; protected routes require a user context.
        Err(SecCtxHttpError::MissingAuthHeader) if is_public => next.run(request).await,
        Err(SecCtxHttpError::MissingAuthHeader) => unauthenticated("MISSING_BEARER"),
        Err(SecCtxHttpError::InvalidAuthHeader | SecCtxHttpError::EmptyToken) => {
            unauthenticated("INVALID_BEARER")
        }
    }
}

/// Map a neutral [`AuthnError`] to a canonical `problem+json` response.
///
/// The token and any provider-specific detail are never surfaced on the wire.
fn authn_error_to_response(err: &AuthnError) -> Response {
    match err {
        // A reachable backend that rejected the token: the caller's credential
        // is bad (401).
        AuthnError::InvalidToken => unauthenticated("AUTHN_FAILED"),
        // The backend could not be reached: surface 503 with a retry hint so
        // callers can distinguish "try later" from "your token is bad".
        AuthnError::Unavailable => CanonicalError::service_unavailable()
            .with_retry_after_seconds(AUTHN_RETRY_AFTER_SECONDS)
            .create()
            .into_response(),
        // `Other` (and, defensively, any future neutral variant) is an
        // unexpected authentication-infrastructure failure, not a bad
        // credential — surface 500 rather than blaming the caller. The
        // diagnostic detail is redacted on the wire by `CanonicalError`.
        // `AuthnError` is `#[non_exhaustive]`, so the wildcard is required.
        _ => CanonicalError::internal(AUTHN_INFRA_FAILURE_DETAIL)
            .create()
            .into_response(),
    }
}

/// Build a canonical `Unauthenticated` (`401`) `problem+json` response with the
/// given machine-readable reason.
fn unauthenticated(reason: &str) -> Response {
    CanonicalError::unauthenticated()
        .with_reason(reason)
        .create()
        .into_response()
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    use axum::{Router, body::Body, routing::get};
    use http::{Request as HttpRequest, StatusCode, header};
    use toolkit_security::SecurityContext;
    use tower::ServiceExt;

    const GOOD_TOKEN: &str = "valid-token";
    const UNAVAILABLE_TOKEN: &str = "unavailable-token";
    const INTERNAL_TOKEN: &str = "internal-failure-token";

    /// Authenticator stand-in: accepts `GOOD_TOKEN`, signals a transient
    /// backend outage for `UNAVAILABLE_TOKEN`, an unexpected infrastructure
    /// failure for `INTERNAL_TOKEN`, and rejects everything else (a forged or
    /// expired JWT).
    struct StubAuthenticator;

    impl BearerAuthenticator for StubAuthenticator {
        async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthnError> {
            match token {
                GOOD_TOKEN => Ok(SecurityContext::anonymous()),
                UNAVAILABLE_TOKEN => Err(AuthnError::Unavailable),
                INTERNAL_TOKEN => Err(AuthnError::Other("boom".to_owned())),
                _ => Err(AuthnError::InvalidToken),
            }
        }
    }

    fn app(is_public: bool) -> Router {
        let authenticator = Arc::new(StubAuthenticator);

        // `secctx_middleware` runs as a `route_layer` (after routing); the
        // `PublicRoute` marker is added as an outer router `layer` so it is
        // present in the request extensions by the time the middleware reads it
        // (this mirrors how OoP-4 surfaces `OperationSpec.is_public` per-route).
        let secctx = axum::middleware::from_fn_with_state(
            authenticator,
            secctx_middleware::<StubAuthenticator>,
        );

        let router = Router::new()
            .route("/", get(|| async { StatusCode::OK }))
            .route_layer(secctx);

        if is_public {
            router.layer(axum::Extension(PublicRoute))
        } else {
            router
        }
    }

    /// Drive a request through `router` and return `(status, content_type)`.
    async fn send(router: Router, auth: Option<&str>) -> (StatusCode, Option<String>) {
        let mut builder = HttpRequest::builder().uri("/");
        if let Some(value) = auth {
            builder = builder.header(header::AUTHORIZATION, value);
        }
        let request = builder.body(Body::empty()).unwrap();
        let response = router.oneshot(request).await.unwrap();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        (response.status(), content_type)
    }

    const PROBLEM_JSON: &str = "application/problem+json";

    #[tokio::test]
    async fn protected_route_without_auth_is_401_problem() {
        let (status, content_type) = send(app(false), None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
    }

    #[tokio::test]
    async fn protected_route_with_valid_token_passes() {
        let (status, _) = send(app(false), Some("Bearer valid-token")).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn forged_token_is_rejected_as_401_problem() {
        let (status, content_type) = send(app(false), Some("Bearer forged-token")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
    }

    #[tokio::test]
    async fn invalid_auth_header_is_401_problem() {
        let (status, content_type) = send(app(false), Some("Basic dXNlcjpwYXNz")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
    }

    #[tokio::test]
    async fn backend_unavailable_is_503_problem() {
        let (status, content_type) = send(app(false), Some("Bearer unavailable-token")).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
    }

    #[tokio::test]
    async fn unexpected_authn_failure_is_500_problem() {
        let (status, content_type) = send(app(false), Some("Bearer internal-failure-token")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
    }

    #[tokio::test]
    async fn public_route_without_auth_passes_through() {
        let (status, _) = send(app(true), None).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn public_route_revalidates_present_token() {
        let (status, _) = send(app(true), Some("Bearer forged-token")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
}
