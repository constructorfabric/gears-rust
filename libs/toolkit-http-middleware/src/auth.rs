//! Axum middleware for two-plane authentication.
//!
//! Two complementary middlewares:
//!
//! - [`security_context_middleware`] (**tenant plane**) resolves the route's
//!   [`AuthRequirement`] via an injected [`RouteAuthPolicy`], then, for
//!   authenticated routes, extracts the bearer token from the incoming
//!   `Authorization` header and **always** re-validates it via an injected
//!   [`BearerAuthenticator`] — there is no trusted-peer fast path (zero-trust).
//!   On success the reconstructed
//!   [`SecurityContext`] is inserted into the
//!   request extensions for downstream handlers and the `AuthZ` resolver. CORS
//!   preflight requests and public routes receive an anonymous
//!   `SecurityContext` and pass through; rejections carry an RFC 6750
//!   `WWW-Authenticate` challenge.
//! - [`internal_auth_middleware`] (**platform plane**) extracts the
//!   `X-ToolKit-Internal-Token` header and, if present, validates it via an
//!   injected [`InternalAuthenticator`], inserting [`PeerAuthenticated`] and a
//!   [`PlatformSecurityContext`] for workload-policy / platform handlers.
//!
//! **Middleware order:** when both are installed, [`internal_auth_middleware`]
//! runs **before** [`security_context_middleware`] (DESIGN § 3.2). The two middlewares are
//! independent: each handles its own plane and the planes are mutually exclusive
//! per request — system calls carry `X-ToolKit-Internal-Token` (no JWT); user
//! calls carry `Authorization: Bearer` (no internal token). [`PeerAuthenticated`]
//! is never a prerequisite for JWT validation; [`security_context_middleware`] does not
//! consult it.
//!
//! Which routes require a tenant JWT is decided by the [`RouteAuthPolicy`]
//! supplied via Axum state at the gear/bootstrap layer; the concrete
//! authenticator adapters are injected the same way.

use std::sync::Arc;

use axum::{
    extract::{MatchedPath, Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use http::{Method, header::ACCESS_CONTROL_REQUEST_METHOD, header::ORIGIN};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::{
    AuthNError, BearerAuthenticator, InternalAuthNError, InternalAuthenticator, PeerAuthenticated,
    PlatformSecurityContext, SecurityContext,
};

use crate::common::{BearerChallenge, append_bearer_challenge, resolve_path};
use crate::policy::{AuthRequirement, RouteAuthPolicy};
use crate::security::{
    InternalTokenHttpError, SecurityContextHttpError, extract_bearer_http,
    extract_internal_token_http,
};
use secrecy::ExposeSecret;

/// Retry hint (seconds) advertised when the authentication backend is
/// temporarily unavailable, mirroring `api-gateway`'s authn middleware.
const AUTH_RETRY_AFTER_SECONDS: u64 = 5;

/// Public detail for an unexpected authentication-infrastructure failure,
/// mirroring `api-gateway`'s authn middleware. Carries no diagnostic specifics.
const AUTH_INFRA_FAILURE_DETAIL: &str = "authentication infrastructure failure";

/// Injected state for [`security_context_middleware`].
///
/// Bundles the concrete [`BearerAuthenticator`] adapter (`Arc<A>`) with the
/// [`RouteAuthPolicy`] that decides which routes require a tenant JWT. Supplied
/// via `axum::middleware::from_fn_with_state` at the gear/bootstrap layer.
pub struct SecurityContextLayerState<A> {
    authenticator: Arc<A>,
    policy: Arc<dyn RouteAuthPolicy>,
    cors_preflight_bypass: bool,
}

impl<A> SecurityContextLayerState<A> {
    /// Build the middleware state from an authenticator and a route policy.
    ///
    /// CORS-preflight bypass is **off** by default: per the edge-architecture
    /// ADR, CORS is an edge concern terminated by the gateway (or an external
    /// gateway), so normal `OoP` gears sitting behind the edge never receive
    /// preflights and stay lightweight. Enable it with
    /// [`with_cors_preflight_bypass`](Self::with_cors_preflight_bypass) at an
    /// edge that installs its own CORS layer.
    pub fn new(authenticator: Arc<A>, policy: Arc<dyn RouteAuthPolicy>) -> Self {
        Self {
            authenticator,
            policy,
            cors_preflight_bypass: false,
        }
    }

    /// Let CORS preflight requests (`OPTIONS` carrying `Origin` +
    /// `Access-Control-Request-Method`) bypass authentication, receiving an
    /// anonymous `SecurityContext` so the CORS layer can answer them.
    ///
    /// This is an **edge** concern — enable it only where a CORS layer is
    /// installed (the api-gateway). Normal gears leave it off.
    #[must_use]
    pub fn with_cors_preflight_bypass(mut self) -> Self {
        self.cors_preflight_bypass = true;
        self
    }
}

// Manual `Clone` so `A` need not be `Clone` (both `Arc` fields plus a `bool`).
// Axum requires the state type to be `Clone`.
impl<A> Clone for SecurityContextLayerState<A> {
    fn clone(&self) -> Self {
        Self {
            authenticator: Arc::clone(&self.authenticator),
            policy: Arc::clone(&self.policy),
            cors_preflight_bypass: self.cors_preflight_bypass,
        }
    }
}

/// Tenant-plane `SecurityContext` middleware.
///
/// Behaviour:
/// - **CORS preflight** (`OPTIONS` carrying `Origin` +
///   `Access-Control-Request-Method`) is never authenticated **when
///   [`with_cors_preflight_bypass`](SecurityContextLayerState::with_cors_preflight_bypass)
///   is enabled** (edge/gateway only): an anonymous
///   [`SecurityContext`] is inserted and the
///   request passes through so the CORS layer can answer it. When disabled (the
///   default for `OoP` gears) a preflight is treated like any other request and
///   subject to the route policy.
/// - The route's [`AuthRequirement`] is resolved via the injected
///   [`RouteAuthPolicy`]. A [`None`](AuthRequirement::None) route receives an
///   anonymous `SecurityContext` and passes through.
/// - For a [`Required`](AuthRequirement::Required) route the bearer token is
///   **always** re-validated via the injected [`BearerAuthenticator`]; on
///   success the reconstructed `SecurityContext` is inserted into request
///   extensions.
/// - A missing credential is `401` (`MISSING_BEARER`) with a
///   `WWW-Authenticate: Bearer realm="api"` challenge; a malformed credential
///   is `401` (`INVALID_BEARER`) with an `invalid_token` challenge; a rejected
///   token is `401` (`AUTHN_FAILED`) with an `invalid_token` challenge; an
///   unreachable backend is `503`; any other unexpected failure is `500`.
///
/// Rejections are rendered as canonical RFC 9457 `application/problem+json`
/// responses (via [`CanonicalError`]) so they match the platform-wide error
/// contract; `instance` / `trace_id` enrichment is left to the outer canonical
/// error middleware installed at the gear/bootstrap layer.
///
/// The handler is generic over `A`; the concrete authenticator and route policy
/// are supplied via [`SecurityContextLayerState`] at the gear/bootstrap layer.
pub async fn security_context_middleware<A>(
    State(state): State<SecurityContextLayerState<A>>,
    mut request: Request,
    next: Next,
) -> Response
where
    A: BearerAuthenticator + 'static,
{
    // CORS preflight bypass is an edge concern (see edge-architecture ADR): only
    // when explicitly enabled do we insert an anonymous context so downstream
    // `Extension<SecurityContext>` extractors don't panic and let the CORS layer
    // answer the preflight. `OoP` gears leave this off and stay lightweight.
    if state.cors_preflight_bypass && is_preflight_request(request.method(), request.headers()) {
        request
            .extensions_mut()
            .insert(SecurityContext::anonymous());
        return next.run(request).await;
    }

    let path = request.extensions().get::<MatchedPath>().map_or_else(
        || request.uri().path().to_owned(),
        |p| p.as_str().to_owned(),
    );
    let path = resolve_path(&request, path.as_str());

    match state.policy.resolve(request.method(), path.as_str()) {
        AuthRequirement::None => {
            request
                .extensions_mut()
                .insert(SecurityContext::anonymous());
            next.run(request).await
        }
        AuthRequirement::Required => match extract_bearer_http(request.headers()) {
            Ok(token) => match state
                .authenticator
                .authenticate(token.expose_secret())
                .await
            {
                Ok(secctx) => {
                    request.extensions_mut().insert(secctx);
                    next.run(request).await
                }
                Err(err) => authn_error_to_response(&err),
            },
            // No credential presented (RFC 6750 §3): challenge without an error code.
            Err(SecurityContextHttpError::MissingAuthHeader) => {
                unauthenticated("MISSING_BEARER", Some(BearerChallenge::NoCredentials))
            }
            // A credential was presented but malformed: treat as invalid_token.
            Err(
                SecurityContextHttpError::InvalidAuthHeader | SecurityContextHttpError::EmptyToken,
            ) => unauthenticated("INVALID_BEARER", Some(BearerChallenge::InvalidToken)),
        },
    }
}

/// Detect a CORS preflight request: an `OPTIONS` carrying both `Origin` and
/// `Access-Control-Request-Method`.
fn is_preflight_request(method: &Method, headers: &http::HeaderMap) -> bool {
    method == Method::OPTIONS
        && headers.contains_key(ORIGIN)
        && headers.contains_key(ACCESS_CONTROL_REQUEST_METHOD)
}

/// Map a neutral [`AuthNError`] to a canonical `problem+json` response.
///
/// The token and any provider-specific detail are never surfaced on the wire.
fn authn_error_to_response(err: &AuthNError) -> Response {
    match err {
        // A reachable backend that rejected the token: the caller's credential
        // is bad (401).
        AuthNError::InvalidToken => {
            tracing::warn!("bearer token rejected: invalid or expired");
            unauthenticated("AUTHN_FAILED", Some(BearerChallenge::InvalidToken))
        }
        // The backend could not be reached: surface 503 with a retry hint so
        // callers can distinguish "try later" from "your token is bad".
        AuthNError::Unavailable => {
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
        // `AuthNError` is `#[non_exhaustive]`, so the wildcard is required.
        _ => {
            tracing::error!("bearer token validation: unexpected infrastructure failure");
            CanonicalError::internal(AUTH_INFRA_FAILURE_DETAIL)
                .create()
                .into_response()
        }
    }
}

/// Build a canonical `Unauthenticated` (`401`) `problem+json` response with the
/// given machine-readable reason and, for tenant-plane bearer rejections, an
/// RFC 6750 `WWW-Authenticate` challenge. Platform-plane (internal-token)
/// rejections pass `None` since they do not use the `Bearer` scheme.
fn unauthenticated(reason: &str, challenge: Option<BearerChallenge>) -> Response {
    let mut response = CanonicalError::unauthenticated()
        .with_reason(reason)
        .create()
        .into_response();
    if let Some(challenge) = challenge {
        append_bearer_challenge(&mut response, challenge);
    }
    response
}

/// Platform-plane internal-auth middleware.
///
/// Behaviour:
/// - When an `X-ToolKit-Internal-Token` header is present, it is validated via
///   the injected [`InternalAuthenticator`]; on success [`PeerAuthenticated`]
///   and a [`PlatformSecurityContext`] are inserted into request extensions.
/// - When the header is **absent**, the request passes through unchanged
///   (permissive): user-only endpoints do not require a system credential, and
///   the tenant plane is enforced independently by [`security_context_middleware`].
/// - When the header is present but **invalid/empty**, or validation fails, the
///   request is **rejected** — so an invalid SA token is turned away before
///   [`security_context_middleware`] (and any handler) runs.
///
/// This sets workload-policy state only; it **never** skips or substitutes for
/// tenant-plane JWT validation. Install this layer so it runs **before**
/// [`security_context_middleware`] (DESIGN § 3.2).
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
        Ok(token) => match authenticator.authenticate(token.expose_secret()).await {
            Ok(identity) => {
                request.extensions_mut().insert(PeerAuthenticated {
                    name: identity.peer_name().to_owned(),
                });
                request
                    .extensions_mut()
                    .insert(PlatformSecurityContext::new(identity));
                next.run(request).await
            }
            Err(err) => internal_authn_error_to_response(&err),
        },
        // No system credential presented: permissive — user-only endpoints do
        // not require one, and the tenant plane is enforced separately.
        Err(InternalTokenHttpError::MissingHeader) => next.run(request).await,
        // A credential was presented but is malformed: reject before the
        // tenant plane runs.
        Err(InternalTokenHttpError::InvalidHeader | InternalTokenHttpError::EmptyToken) => {
            unauthenticated("INVALID_INTERNAL_TOKEN", None)
        }
    }
}

/// Map a neutral [`InternalAuthNError`] to a canonical `problem+json` response.
///
/// The token and any provider-specific detail are never surfaced on the wire.
fn internal_authn_error_to_response(err: &InternalAuthNError) -> Response {
    match err {
        // A reachable backend that rejected the credential: it is bad (401).
        InternalAuthNError::InvalidToken => {
            tracing::warn!("internal token rejected: invalid or expired credential");
            unauthenticated("INTERNAL_AUTH_FAILED", None)
        }
        // The validation backend (e.g. K8s TokenReview) was unreachable: 503.
        InternalAuthNError::Unavailable => {
            tracing::warn!("internal token validation: authentication backend unavailable");
            CanonicalError::service_unavailable()
                .with_retry_after_seconds(AUTH_RETRY_AFTER_SECONDS)
                .create()
                .into_response()
        }
        // `Other` (and, defensively, any future neutral variant) is an
        // unexpected infrastructure failure — surface 500. `InternalAuthNError`
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
#[path = "auth_tests.rs"]
mod tests;
