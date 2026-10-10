//! Axum middleware for two-plane authentication.
//!
//! Two complementary middlewares:
//!
//! - [`security_context_middleware`] (**tenant plane**) extracts the bearer token from
//!   the incoming `Authorization` header and **always** re-validates it via an
//!   injected [`BearerAuthenticator`] — there is no trusted-peer fast path
//!   (zero-trust). On success the reconstructed [`SecurityContext`](toolkit_security::SecurityContext)
//!   is inserted into the request extensions for downstream handlers and the
//!   `AuthZ` resolver.
//! - [`internal_auth_middleware`] (**platform plane**) extracts the
//!   `X-ToolKit-Internal-Token` header and, if present, validates it via an
//!   injected [`InternalAuthenticator`], inserting [`PeerAuthenticated`] and a
//!   [`PlatformSecurityContext`] for workload-policy / platform handlers.
//!
//! **Middleware order:** install the stack with [`layer_route_auth`], which applies
//! [`route_auth_middleware`] (policy) → [`internal_auth_middleware`] →
//! [`security_context_middleware`] → [`platform_route_middleware`] (gate) → handler
//! (DESIGN § 3.2). Platform routes fail closed only when the gate sits inside both
//! planes, so prefer the helper over wiring the four layers by hand. The two planes are
//! independent: each handles its own credential, and [`PeerAuthenticated`] is never a
//! prerequisite for JWT validation; [`security_context_middleware`] does not consult it.
//!
//! Routes that carry no tenant JWT (probes, platform-plane-only handlers) are
//! marked with the [`AnonymousRoute`] request extension by the gear/bootstrap
//! layer; note this is distinct from `OperationSpec.exposed`, which controls
//! gateway registration. The concrete authenticator adapters are injected via
//! Axum state at the same layer.

use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    Router,
    extract::{MatchedPath, Request, State},
    middleware::{Next, from_fn, from_fn_with_state},
    response::{IntoResponse, Response},
};
use http::{Method, header::AUTHORIZATION};
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_security::{
    AuthNError, BearerAuthenticator, DynBearerAuthenticator, DynInternalAuthenticator,
    InternalAuthNError, InternalAuthenticator, PeerAuthenticated, PlatformAuthEnforced,
    PlatformSecurityContext, SecurityContext,
};

/// Listener auth policy derived from `OperationSpec`, installed before both auth planes.
pub use toolkit_security::RouteAuth;

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

/// Per-route marker indicating the route carries **no tenant JWT** and must not
/// require a [`SecurityContext`](toolkit_security::SecurityContext).
///
/// Inserted by the gear/bootstrap layer for routes that never carry an
/// `Authorization: Bearer` header — framework probe endpoints (`/healthz`,
/// `/readyz`), and platform-plane-only handlers that authenticate via
/// `X-ToolKit-Internal-Token` instead. When present, [`security_context_middleware`] lets
/// a request without an `Authorization` header pass through instead of
/// returning `401`.
///
/// **Not the same as `OperationSpec.exposed`.** `exposed` controls whether
/// a route is registered in the gateway for external access; this marker
/// controls whether [`security_context_middleware`] requires a JWT. The two are
/// independent: most gateway-exposed routes DO carry a JWT and do NOT need this
/// marker; most probe routes are NOT gateway-exposed but DO need it.
#[derive(Clone, Copy, Debug)]
pub struct AnonymousRoute;

/// Auth by method and exact [`MatchedPath`] template; undeclared HEAD inherits GET.
#[derive(Clone, Debug, Default)]
pub struct RouteAuthPolicy {
    routes: HashMap<Method, HashMap<String, RouteAuth>>,
}

impl RouteAuthPolicy {
    /// Record `auth` for `method` on the path template `path`.
    pub fn insert(&mut self, method: Method, path: impl Into<String>, auth: RouteAuth) {
        self.routes
            .entry(method)
            .or_default()
            .insert(path.into(), auth);
    }

    /// The policy recorded for `method` on the matched path template `path`.
    #[must_use]
    pub fn resolve(&self, method: &Method, path: &str) -> Option<RouteAuth> {
        let lookup = |m: &Method| {
            self.routes
                .get(m)
                .and_then(|paths| paths.get(path))
                .copied()
        };
        lookup(method).or_else(|| {
            if *method == Method::HEAD {
                lookup(&Method::GET)
            } else {
                None
            }
        })
    }

    /// `METHOD path` of every [`RouteAuth::Platform`] route, sorted.
    #[must_use]
    pub fn platform_routes(&self) -> Vec<String> {
        let mut routes: Vec<String> = self
            .routes
            .iter()
            .flat_map(|(method, paths)| {
                paths
                    .iter()
                    .filter(|(_, auth)| **auth == RouteAuth::Platform)
                    .map(move |(path, _)| format!("{method} {path}"))
            })
            .collect();
        routes.sort_unstable();
        routes
    }
}

impl FromIterator<(Method, String, RouteAuth)> for RouteAuthPolicy {
    fn from_iter<I: IntoIterator<Item = (Method, String, RouteAuth)>>(iter: I) -> Self {
        let mut policy = Self::default();
        for (method, path, auth) in iter {
            policy.insert(method, path, auth);
        }
        policy
    }
}

/// Layer the route policy, both auth planes and the platform gate onto `router` in the order
/// `cpt-cf-adr-two-plane-auth` requires: policy → platform plane → tenant plane → gate → handler.
///
/// A missing authenticator leaves its plane uninstalled. Platform routes still fail closed
/// (the gate is always installed), and a warning names them when no internal authenticator
/// can admit a caller.
pub fn layer_route_auth<S>(
    router: Router<S>,
    policy: RouteAuthPolicy,
    bearer: Option<DynBearerAuthenticator>,
    internal: Option<DynInternalAuthenticator>,
) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    let mut router = router.layer(from_fn(platform_route_middleware));
    if let Some(bearer) = bearer {
        router = router.layer(from_fn_with_state(
            Arc::new(bearer),
            security_context_middleware::<DynBearerAuthenticator>,
        ));
    }
    if let Some(internal) = internal {
        router = router.layer(from_fn_with_state(
            Arc::new(internal),
            internal_auth_middleware::<DynInternalAuthenticator>,
        ));
    } else {
        let platform_routes = policy.platform_routes();
        if !platform_routes.is_empty() {
            tracing::warn!(
                routes = ?platform_routes,
                "platform routes are declared but no internal authenticator is installed; \
                 every request to them will be refused with 401"
            );
        }
    }
    router.layer(from_fn_with_state(Arc::new(policy), route_auth_middleware))
}

/// Install route policy outside both auth planes as a router layer; `MethodRouter` is too late.
/// Unlisted routes require a bearer. Anonymous routes get an anonymous
/// [`SecurityContext`] that a validated bearer replaces, so handlers extracting
/// `Extension<SecurityContext>` work without credentials and without a tenant plane.
pub async fn route_auth_middleware(
    State(policy): State<Arc<RouteAuthPolicy>>,
    mut request: Request,
    next: Next,
) -> Response {
    let auth = request
        .extensions()
        .get::<MatchedPath>()
        .and_then(|path| policy.resolve(request.method(), path.as_str()));
    if let Some(auth) = auth {
        if auth == RouteAuth::Anonymous {
            request.extensions_mut().insert(AnonymousRoute);
            request
                .extensions_mut()
                .insert(SecurityContext::anonymous());
        }
        request.extensions_mut().insert(auth);
    }
    next.run(request).await
}

/// Always install inside both auth planes; applies only to [`RouteAuth::Platform`].
/// Requires a validated platform caller and rejects any unvalidated presented credential with 401.
/// Handlers receive `Extension<PlatformSecurityContext>`.
pub async fn platform_route_middleware(request: Request, next: Next) -> Response {
    if request.extensions().get::<RouteAuth>() != Some(&RouteAuth::Platform) {
        return next.run(request).await;
    }
    if let Err(refusal) = admit_platform(&request) {
        return refusal.into_response();
    }
    next.run(request).await
}

/// Canonical 401 refusal from [`admit_platform`], identified by [`Self::reason`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlatformRefusal {
    /// A bearer was presented that no installed plane validated.
    UnverifiedBearer,
    /// An internal token was presented that no installed plane validated.
    UnverifiedInternalToken,
    /// No validated platform caller.
    MissingInternalToken,
}

impl PlatformRefusal {
    /// The `401`'s machine-readable reason.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        match self {
            Self::UnverifiedBearer => "UNVERIFIED_BEARER",
            Self::UnverifiedInternalToken => "UNVERIFIED_INTERNAL_TOKEN",
            Self::MissingInternalToken => "MISSING_INTERNAL_TOKEN",
        }
    }
}

impl IntoResponse for PlatformRefusal {
    fn into_response(self) -> Response {
        unauthenticated(self.reason())
    }
}

/// Check platform admission and log refusals at info, with the caller's subject when a
/// validated tenant context is present.
///
/// # Errors
/// [`PlatformRefusal`] for unvalidated required credentials.
pub fn admit_platform(request: &Request) -> Result<(), PlatformRefusal> {
    let extensions = request.extensions();
    let tenant = extensions
        .get::<SecurityContext>()
        .is_some_and(|ctx| !ctx.is_anonymous());
    let platform = extensions
        .get::<PlatformSecurityContext>()
        .is_some_and(|ctx| !ctx.is_outbound_marker());

    let refusal = if request.headers().contains_key(AUTHORIZATION) && !tenant {
        PlatformRefusal::UnverifiedBearer
    } else if request.headers().contains_key(INTERNAL_TOKEN_HEADER) && !platform {
        PlatformRefusal::UnverifiedInternalToken
    } else if !platform {
        PlatformRefusal::MissingInternalToken
    } else {
        return Ok(());
    };
    let route = extensions
        .get::<MatchedPath>()
        .map_or_else(|| request.uri().path(), MatchedPath::as_str);
    let subject_id = extensions
        .get::<SecurityContext>()
        .filter(|ctx| !ctx.is_anonymous())
        .map(SecurityContext::subject_id);
    tracing::info!(
        method = %request.method(),
        route,
        reason = refusal.reason(),
        subject_id = ?subject_id,
        "platform route refused"
    );
    Err(refusal)
}

/// Tenant-plane `SecurityContext` middleware.
///
/// Behaviour:
/// - A bearer token, if present, is **always** re-validated via the injected
///   [`BearerAuthenticator`]; on success the [`SecurityContext`](toolkit_security::SecurityContext) is inserted
///   into request extensions.
/// - A protected route (no [`AnonymousRoute`] marker) with a missing or invalid
///   `Authorization` header is rejected with `401`.
/// - An anonymous / system-only route (carrying the [`AnonymousRoute`] marker)
///   with no `Authorization` header passes through.
/// - Without Authorization, [`RouteAuth::Platform`] delegates to [`platform_route_middleware`].
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
///
/// TODO: Rework to align with the gateway's `authn_middleware`
/// (`gears/system/api-gateway/src/middleware/auth.rs`), the more mature
/// implementation: route-policy-driven auth requirements (vs. the binary
/// `AnonymousRoute` marker), CORS-preflight handling, anonymous-`SecurityContext`
/// insertion for public routes, and RFC 6750 `WWW-Authenticate` Bearer
/// challenges. Part of consolidating all gateway middlewares into this crate.
pub async fn security_context_middleware<A>(
    State(authenticator): State<Arc<A>>,
    mut request: Request,
    next: Next,
) -> Response
where
    A: BearerAuthenticator + 'static,
{
    // Without a bearer, the inner platform gate decides admission.
    let is_anonymous = request.extensions().get::<AnonymousRoute>().is_some()
        || request.extensions().get::<RouteAuth>() == Some(&RouteAuth::Platform);

    match extract_bearer_http(request.headers()) {
        Ok(token) => match authenticator.authenticate(token.expose_secret()).await {
            Ok(secctx) => {
                request.extensions_mut().insert(secctx);
                next.run(request).await
            }
            Err(err) => authn_error_to_response(&err),
        },
        // No credential presented: allow through only for anonymous/system-only
        // routes; protected routes require a user context.
        Err(SecurityContextHttpError::MissingAuthHeader) if is_anonymous => next.run(request).await,
        Err(SecurityContextHttpError::MissingAuthHeader) => unauthenticated("MISSING_BEARER"),
        Err(SecurityContextHttpError::InvalidAuthHeader | SecurityContextHttpError::EmptyToken) => {
            unauthenticated("INVALID_BEARER")
        }
    }
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
            unauthenticated("AUTHN_FAILED")
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
///   the tenant plane is enforced independently by [`security_context_middleware`].
/// - When the header is present but **invalid/empty**, or validation fails, the
///   request is **rejected** — so an invalid SA token is turned away before
///   [`security_context_middleware`] (and any handler) runs.
///
/// Installing the middleware means platform auth is active on the listener, so
/// it stamps [`PlatformAuthEnforced`] on every request it forwards — the
/// posture signal a handler uses to fail closed on an anonymous caller (see
/// that type).
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
    // Stamp the posture marker before token extraction, so it rides every
    // request this active listener forwards regardless of the token outcome.
    request.extensions_mut().insert(PlatformAuthEnforced);

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
            unauthenticated("INVALID_INTERNAL_TOKEN")
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
            unauthenticated("INTERNAL_AUTH_FAILED")
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
