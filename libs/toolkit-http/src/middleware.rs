//! Axum middleware for two-plane authentication.
//!
//! Gated behind the `axum-middleware` feature. Two complementary middlewares:
//!
//! - [`secctx_middleware`] (**tenant plane**) extracts the bearer token from
//!   the incoming `Authorization` header and **always** re-validates it via an
//!   injected [`BearerAuthenticator`] — there is no trusted-peer fast path
//!   (zero-trust). On success the reconstructed [`SecurityContext`]
//!   is inserted into the request extensions for downstream handlers and the
//!   `AuthZ` resolver.
//! - [`internal_auth_middleware`] (**platform plane**) extracts the
//!   `X-ToolKit-Internal-Token` header and, if present, validates it via an
//!   injected [`InternalAuthenticator`], inserting [`PeerAuthenticated`] and a
//!   [`PlatformSecurityContext`] for workload-policy / platform handlers.
//!
//! **Middleware order:** when both are installed, [`internal_auth_middleware`]
//! runs **before** [`secctx_middleware`] (DESIGN § 3.2). The two middlewares are
//! independent: each handles its own plane and the planes are mutually exclusive
//! per request — system calls carry `X-ToolKit-Internal-Token` (no JWT); user
//! calls carry `Authorization: Bearer` (no internal token). [`PeerAuthenticated`]
//! is never a prerequisite for JWT validation; [`secctx_middleware`] does not
//! consult it.
//!
//! Routes that carry no tenant JWT (probes, platform-plane-only handlers) are
//! marked with the [`PublicRoute`] request extension by the gear/bootstrap layer;
//! note this is distinct from `OperationSpec.is_public`, which controls gateway
//! registration. The concrete authenticator adapters are injected via Axum state
//! at the same layer.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{
    AuthnError, BearerAuthenticator, InternalAuthError, InternalAuthenticator, PeerAuthenticated,
    PlatformSecurityContext,
};

use crate::security::{
    InternalTokenHttpError, SecCtxHttpError, extract_bearer_http, extract_internal_token_http,
};
use secrecy::ExposeSecret;

/// Retry hint (seconds) advertised when the authentication backend is
/// temporarily unavailable, mirroring `api-gateway`'s authn middleware.
const AUTH_RETRY_AFTER_SECONDS: u64 = 5;

/// Public detail for an unexpected authentication-infrastructure failure,
/// mirroring `api-gateway`'s authn middleware. Carries no diagnostic specifics.
const AUTH_INFRA_FAILURE_DETAIL: &str = "authentication infrastructure failure";

/// Per-route marker indicating the route carries **no tenant JWT** and must not
/// require a [`SecurityContext`].
///
/// Inserted by the gear/bootstrap layer for routes that never carry an
/// `Authorization: Bearer` header — framework probe endpoints (`/healthz`,
/// `/readyz`), and platform-plane-only handlers that authenticate via
/// `X-ToolKit-Internal-Token` instead. When present, [`secctx_middleware`] lets
/// a request without an `Authorization` header pass through instead of
/// returning `401`.
///
/// **Not the same as `OperationSpec.is_public`.** `is_public` controls whether
/// a route is registered in the gateway for external access; this marker
/// controls whether [`secctx_middleware`] requires a JWT. The two are
/// independent: most gateway-exposed routes DO carry a JWT and do NOT need this
/// marker; most probe routes are NOT gateway-exposed but DO need it.
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
        Ok(token) => match authenticator.authenticate(token.expose_secret()).await {
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
        AuthnError::InvalidToken => {
            tracing::warn!("bearer token rejected: invalid or expired");
            unauthenticated("AUTHN_FAILED")
        }
        // The backend could not be reached: surface 503 with a retry hint so
        // callers can distinguish "try later" from "your token is bad".
        AuthnError::Unavailable => {
            tracing::warn!("bearer token validation: authentication backend unavailable");
            CanonicalError::service_unavailable()
                .with_retry_after_seconds(AUTH_RETRY_AFTER_SECONDS)
                .create()
                .into_response()
        }
        // `Other` (and, defensively, any future neutral variant) is an
        // unexpected authentication-infrastructure failure, not a bad
        // credential — surface 500 rather than blaming the caller. The
        // diagnostic detail is redacted on the wire by `CanonicalError`.
        // `AuthnError` is `#[non_exhaustive]`, so the wildcard is required.
        _ => {
            tracing::error!("bearer token validation: unexpected infrastructure failure");
            CanonicalError::internal(AUTH_INFRA_FAILURE_DETAIL)
                .create()
                .into_response()
        }
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

/// Platform-plane internal-auth middleware.
///
/// Behaviour:
/// - When an `X-ToolKit-Internal-Token` header is present, it is validated via
///   the injected [`InternalAuthenticator`]; on success [`PeerAuthenticated`]
///   and a [`PlatformSecurityContext`] are inserted into request extensions.
/// - When the header is **absent**, the request passes through unchanged
///   (permissive): user-only endpoints do not require a system credential, and
///   the tenant plane is enforced independently by [`secctx_middleware`].
/// - When the header is present but **invalid/empty**, or validation fails, the
///   request is **rejected** — so an invalid SA token is turned away before
///   [`secctx_middleware`] (and any handler) runs.
///
/// This sets workload-policy state only; it **never** skips or substitutes for
/// tenant-plane JWT validation. Install this layer so it runs **before**
/// [`secctx_middleware`] (DESIGN § 3.2).
///
/// Rejections are rendered as canonical RFC 9457 `application/problem+json`:
/// an invalid credential is `401`, an unreachable validation backend is `503`,
/// and any other unexpected failure is `500`.
///
/// The handler is generic over `A`; the concrete validator (K8s `TokenReview`
/// in the first phase) is supplied via Axum state as `Arc<A>` at the
/// gear/bootstrap layer.
pub async fn internal_auth_middleware<A>(
    State(authenticator): State<Arc<A>>,
    mut request: Request,
    next: Next,
) -> Response
where
    A: InternalAuthenticator + 'static,
{
    match extract_internal_token_http(request.headers()) {
        Ok(token) => match authenticator.validate(token.expose_secret()).await {
            Ok(identity) => {
                request.extensions_mut().insert(PeerAuthenticated {
                    gear: identity.peer_gear().to_owned(),
                });
                request
                    .extensions_mut()
                    .insert(PlatformSecurityContext::new(identity));
                next.run(request).await
            }
            Err(err) => internal_auth_error_to_response(&err),
        },
        // No system credential presented: permissive — user-only endpoints do
        // not require one, and the tenant plane is enforced separately.
        Err(InternalTokenHttpError::MissingHeader) => next.run(request).await,
        // A credential was presented but is malformed: reject before the
        // tenant plane runs.
        Err(InternalTokenHttpError::InvalidHeader | InternalTokenHttpError::EmptyToken) => {
            unauthenticated("INVALID_INTERNAL_TOKEN")
        }
    }
}

/// Map a neutral [`InternalAuthError`] to a canonical `problem+json` response.
///
/// The token and any provider-specific detail are never surfaced on the wire.
fn internal_auth_error_to_response(err: &InternalAuthError) -> Response {
    match err {
        // A reachable backend that rejected the credential: it is bad (401).
        InternalAuthError::InvalidToken => {
            tracing::warn!("internal token rejected: invalid or expired credential");
            unauthenticated("INTERNAL_AUTH_FAILED")
        }
        // The validation backend (e.g. K8s TokenReview) was unreachable: 503.
        InternalAuthError::Unavailable => {
            tracing::warn!("internal token validation: authentication backend unavailable");
            CanonicalError::service_unavailable()
                .with_retry_after_seconds(AUTH_RETRY_AFTER_SECONDS)
                .create()
                .into_response()
        }
        // `Other` (and, defensively, any future neutral variant) is an
        // unexpected infrastructure failure — surface 500. `InternalAuthError`
        // is `#[non_exhaustive]`, so the wildcard is required.
        _ => {
            tracing::error!("internal token validation: unexpected infrastructure failure");
            CanonicalError::internal(AUTH_INFRA_FAILURE_DETAIL)
                .create()
                .into_response()
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    use axum::{
        Extension, Router,
        body::{Body, to_bytes},
        routing::get,
    };
    use http::{Request as HttpRequest, StatusCode, header};
    use toolkit_security::{
        InternalAuthError, InternalAuthenticator, PlatformIdentity, SecurityContext,
    };
    use tower::ServiceExt;

    const GOOD_TOKEN: &str = "valid-token";
    const UNAVAILABLE_TOKEN: &str = "unavailable-token";
    const INTERNAL_TOKEN: &str = "internal-failure-token";
    const INTERNAL_HEADER: &str = "x-toolkit-internal-token";
    const SA_GOOD: &str = "good-sa-token";
    const SA_UNAVAILABLE: &str = "unavailable-sa-token";
    const SA_INTERNAL: &str = "internal-failure-sa-token";
    const PROBLEM_JSON: &str = "application/problem+json";

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
        // (this mirrors how the bootstrap layer surfaces `OperationSpec.is_public` per-route).
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

    /// Internal-auth stand-in: accepts `SA_GOOD` (as `flight-control`), signals
    /// outage for `SA_UNAVAILABLE`, an unexpected failure for `SA_INTERNAL`, and
    /// rejects everything else.
    struct StubInternalAuthenticator;

    impl InternalAuthenticator for StubInternalAuthenticator {
        async fn validate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthError> {
            match token {
                SA_GOOD => Ok(PlatformIdentity::ServiceAccount {
                    namespace: "toolkit".to_owned(),
                    service_account: "flight-control".to_owned(),
                    pod: None,
                }),
                SA_UNAVAILABLE => Err(InternalAuthError::Unavailable),
                SA_INTERNAL => Err(InternalAuthError::Other("boom".to_owned())),
                _ => Err(InternalAuthError::InvalidToken),
            }
        }
    }

    /// Handler that echoes the authenticated peer gear, but only when **both**
    /// the [`PeerAuthenticated`] marker and the [`PlatformSecurityContext`] are
    /// present in the request extensions (otherwise `"none"`).
    async fn peer_echo(
        peer: Option<Extension<PeerAuthenticated>>,
        platform: Option<Extension<PlatformSecurityContext>>,
    ) -> String {
        match (peer, platform) {
            (Some(Extension(peer)), Some(_)) => peer.gear,
            _ => "none".to_owned(),
        }
    }

    fn platform_app() -> Router {
        let authenticator = Arc::new(StubInternalAuthenticator);
        let layer = axum::middleware::from_fn_with_state(
            authenticator,
            internal_auth_middleware::<StubInternalAuthenticator>,
        );
        Router::new().route("/", get(peer_echo)).route_layer(layer)
    }

    /// Stacked app: `internal_auth_middleware` (outermost, runs first) then
    /// `secctx_middleware`, mirroring the DESIGN § 3.2 middleware order.
    fn stacked_app() -> Router {
        let bearer = Arc::new(StubAuthenticator);
        let internal = Arc::new(StubInternalAuthenticator);
        let secctx =
            axum::middleware::from_fn_with_state(bearer, secctx_middleware::<StubAuthenticator>);
        let internal_layer = axum::middleware::from_fn_with_state(
            internal,
            internal_auth_middleware::<StubInternalAuthenticator>,
        );
        Router::new()
            .route("/", get(|| async { StatusCode::OK }))
            .route_layer(secctx)
            .route_layer(internal_layer)
    }

    fn stacked_public_app() -> Router {
        let bearer = Arc::new(StubAuthenticator);
        let internal = Arc::new(StubInternalAuthenticator);
        let secctx =
            axum::middleware::from_fn_with_state(bearer, secctx_middleware::<StubAuthenticator>);
        let internal_layer = axum::middleware::from_fn_with_state(
            internal,
            internal_auth_middleware::<StubInternalAuthenticator>,
        );
        Router::new()
            .route("/", get(|| async { StatusCode::OK }))
            .route_layer(secctx)
            .route_layer(internal_layer)
            .layer(axum::Extension(PublicRoute))
    }

    /// Drive a request with arbitrary headers through `router`, returning
    /// `(status, content_type, body)`.
    async fn send_headers(
        router: Router,
        headers: &[(&str, &str)],
    ) -> (StatusCode, Option<String>, String) {
        let mut builder = HttpRequest::builder().uri("/");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder.body(Body::empty()).unwrap();
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (
            status,
            content_type,
            String::from_utf8_lossy(&bytes).into_owned(),
        )
    }

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

    #[tokio::test]
    async fn internal_no_header_passes_through_permissive() {
        let (status, _, body) = send_headers(platform_app(), &[]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "none");
    }

    #[tokio::test]
    async fn internal_valid_token_sets_peer_and_platform_context() {
        let (status, _, body) = send_headers(platform_app(), &[(INTERNAL_HEADER, SA_GOOD)]).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "flight-control");
    }

    #[tokio::test]
    async fn internal_invalid_token_is_401_problem() {
        let (status, content_type, _) =
            send_headers(platform_app(), &[(INTERNAL_HEADER, "forged")]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
    }

    #[tokio::test]
    async fn internal_backend_unavailable_is_503_problem() {
        let (status, content_type, _) =
            send_headers(platform_app(), &[(INTERNAL_HEADER, SA_UNAVAILABLE)]).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
    }

    #[tokio::test]
    async fn internal_unexpected_failure_is_500_problem() {
        let (status, content_type, _) =
            send_headers(platform_app(), &[(INTERNAL_HEADER, SA_INTERNAL)]).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
    }

    #[tokio::test]
    async fn internal_empty_header_is_401_problem() {
        let (status, content_type, _) =
            send_headers(platform_app(), &[(INTERNAL_HEADER, "   ")]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(content_type.as_deref(), Some(PROBLEM_JSON));
    }

    #[tokio::test]
    async fn peer_authenticated_does_not_skip_jwt_validation() {
        // Valid SA token (peer authenticated) but a forged user JWT: the tenant
        // plane must still reject — peer trust is not a JWT fast path.
        let (status, _, _) = send_headers(
            stacked_app(),
            &[
                (INTERNAL_HEADER, SA_GOOD),
                (header::AUTHORIZATION.as_str(), "Bearer forged-token"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn valid_peer_and_valid_jwt_passes() {
        let (status, _, _) = send_headers(
            stacked_app(),
            &[
                (INTERNAL_HEADER, SA_GOOD),
                (header::AUTHORIZATION.as_str(), "Bearer valid-token"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn invalid_internal_token_rejected_before_tenant_plane() {
        // A bad SA token must be turned away by internal_auth_middleware before
        // secctx_middleware runs — even though the user JWT here is valid.
        let (status, _, _) = send_headers(
            stacked_app(),
            &[
                (INTERNAL_HEADER, "forged"),
                (header::AUTHORIZATION.as_str(), "Bearer valid-token"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn system_call_to_public_endpoint_passes() {
        // Valid SA token, no JWT, route is PublicRoute — the normal probe/platform path.
        let (status, _, _) =
            send_headers(stacked_public_app(), &[(INTERNAL_HEADER, SA_GOOD)]).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn public_endpoint_with_no_credentials_passes() {
        // No SA token and no JWT on a PublicRoute: passes (health probe).
        let (status, _, _) = send_headers(stacked_public_app(), &[]).await;
        assert_eq!(status, StatusCode::OK);
    }
}
