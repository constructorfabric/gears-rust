use super::*;

use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    routing::get,
};
use http::{Method, Request as HttpRequest, StatusCode, header};
use toolkit_security::{
    InternalAuthNError, InternalAuthenticator, PlatformIdentity, SecurityContext,
};
use tower::ServiceExt;

use crate::policy::{AuthRequirement, RouteAuthPolicy};

/// Route policy stand-in returning a fixed [`AuthRequirement`] for every route.
struct StubPolicy(AuthRequirement);

impl RouteAuthPolicy for StubPolicy {
    fn resolve(&self, _method: &Method, _path: &str) -> AuthRequirement {
        self.0
    }
}

/// Build tenant-plane middleware state for the given authenticator + requirement.
fn secctx_state(requirement: AuthRequirement) -> SecurityContextLayerState<StubAuthenticator> {
    let policy: Arc<dyn RouteAuthPolicy> = Arc::new(StubPolicy(requirement));
    SecurityContextLayerState::new(Arc::new(StubAuthenticator), policy)
}

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
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        match token {
            GOOD_TOKEN => Ok(SecurityContext::anonymous()),
            UNAVAILABLE_TOKEN => Err(AuthNError::Unavailable),
            INTERNAL_TOKEN => Err(AuthNError::Other("boom".to_owned())),
            _ => Err(AuthNError::InvalidToken),
        }
    }
}

fn app(is_public: bool) -> Router {
    // The route policy decides whether `/` requires a JWT; a public route
    // resolves to `AuthRequirement::None`, a protected one to `Required`.
    let requirement = if is_public {
        AuthRequirement::None
    } else {
        AuthRequirement::Required
    };
    let secctx = axum::middleware::from_fn_with_state(
        secctx_state(requirement),
        security_context_middleware::<StubAuthenticator>,
    );

    Router::new()
        .route("/", get(|| async { StatusCode::OK }))
        .route_layer(secctx)
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
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        match token {
            SA_GOOD => Ok(PlatformIdentity::KubernetesServiceAccount {
                namespace: "toolkit".to_owned(),
                service_account: "flight-control".to_owned(),
                pod: None,
            }),
            SA_UNAVAILABLE => Err(InternalAuthNError::Unavailable),
            SA_INTERNAL => Err(InternalAuthNError::Other("boom".to_owned())),
            _ => Err(InternalAuthNError::InvalidToken),
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
        (Some(Extension(peer)), Some(_)) => peer.name,
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
/// `security_context_middleware`, mirroring the DESIGN § 3.2 middleware order.
fn stacked_app() -> Router {
    let internal = Arc::new(StubInternalAuthenticator);
    let secctx = axum::middleware::from_fn_with_state(
        secctx_state(AuthRequirement::Required),
        security_context_middleware::<StubAuthenticator>,
    );
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
    let internal = Arc::new(StubInternalAuthenticator);
    let secctx = axum::middleware::from_fn_with_state(
        secctx_state(AuthRequirement::None),
        security_context_middleware::<StubAuthenticator>,
    );
    let internal_layer = axum::middleware::from_fn_with_state(
        internal,
        internal_auth_middleware::<StubInternalAuthenticator>,
    );
    Router::new()
        .route("/", get(|| async { StatusCode::OK }))
        .route_layer(secctx)
        .route_layer(internal_layer)
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
async fn public_route_ignores_present_token() {
    // Converged (gateway) behaviour: a `None` route is never authenticated, so
    // even a forged token is ignored and the request passes through with an
    // anonymous `SecurityContext`.
    let (status, _) = send(app(true), Some("Bearer forged-token")).await;
    assert_eq!(status, StatusCode::OK);
}

/// Build a `Required`-policy app whose CORS-preflight bypass is toggled.
fn preflight_app(bypass: bool) -> Router {
    let policy: Arc<dyn RouteAuthPolicy> = Arc::new(StubPolicy(AuthRequirement::Required));
    let mut state = SecurityContextLayerState::new(Arc::new(StubAuthenticator), policy);
    if bypass {
        state = state.with_cors_preflight_bypass();
    }
    let secctx = axum::middleware::from_fn_with_state(
        state,
        security_context_middleware::<StubAuthenticator>,
    );
    Router::new()
        .route(
            "/",
            get(|| async { StatusCode::OK }).options(|| async { StatusCode::OK }),
        )
        .route_layer(secctx)
}

/// Drive a CORS preflight (`OPTIONS` + `Origin` + `Access-Control-Request-Method`).
async fn send_preflight(router: Router) -> StatusCode {
    let request = HttpRequest::builder()
        .method(Method::OPTIONS)
        .uri("/")
        .header(header::ORIGIN, "https://example.com")
        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "GET")
        .body(Body::empty())
        .unwrap();
    router.oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn cors_preflight_bypasses_auth_when_enabled() {
    // Edge/gateway behaviour: preflight skips auth and reaches the handler.
    assert_eq!(send_preflight(preflight_app(true)).await, StatusCode::OK);
}

#[tokio::test]
async fn cors_preflight_enforced_by_default() {
    // Default (OoP gears): CORS bypass is edge-only, so a preflight to a
    // `Required` route with no token is rejected like any other request.
    assert_eq!(
        send_preflight(preflight_app(false)).await,
        StatusCode::UNAUTHORIZED
    );
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
    let (status, content_type, _) = send_headers(platform_app(), &[(INTERNAL_HEADER, "   ")]).await;
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
    // security_context_middleware runs — even though the user JWT here is valid.
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
    // Valid SA token, no JWT, public route — the normal probe/platform path.
    let (status, _, _) = send_headers(stacked_public_app(), &[(INTERNAL_HEADER, SA_GOOD)]).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn public_endpoint_with_no_credentials_passes() {
    // No SA token and no JWT on a public route: passes (health probe).
    let (status, _, _) = send_headers(stacked_public_app(), &[]).await;
    assert_eq!(status, StatusCode::OK);
}

/// App driven by the real [`MatchitRouteAuthPolicy`] (the tests above use a
/// fixed `StubPolicy`; these exercise per-route matching): `/public` is
/// explicitly public, `/protected` explicitly authenticated, and `/other` is
/// unmatched (governed by `require_auth_by_default`).
fn matchit_policy_app(require_auth_by_default: bool) -> Router {
    let authenticated: std::collections::HashSet<(Method, String)> =
        [(Method::GET, "/protected".to_owned())]
            .into_iter()
            .collect();
    let public: std::collections::HashSet<(Method, String)> =
        [(Method::GET, "/public".to_owned())].into_iter().collect();
    let policy: Arc<dyn RouteAuthPolicy> = Arc::new(
        crate::MatchitRouteAuthPolicy::from_route_sets(
            authenticated,
            public,
            require_auth_by_default,
        )
        .expect("valid route patterns"),
    );
    let secctx = axum::middleware::from_fn_with_state(
        SecurityContextLayerState::new(Arc::new(StubAuthenticator), policy),
        security_context_middleware::<StubAuthenticator>,
    );
    Router::new()
        .route("/public", get(|| async { StatusCode::OK }))
        .route("/protected", get(|| async { StatusCode::OK }))
        .route("/other", get(|| async { StatusCode::OK }))
        .route_layer(secctx)
}

/// Drive a `GET path` through `router` and return the response status.
async fn get_status(router: Router, path: &str, auth: Option<&str>) -> StatusCode {
    let mut builder = HttpRequest::builder().uri(path).method(Method::GET);
    if let Some(value) = auth {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    let request = builder.body(Body::empty()).unwrap();
    router.oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn matchit_policy_public_route_passes_without_token() {
    assert_eq!(
        get_status(matchit_policy_app(true), "/public", None).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn matchit_policy_protected_route_rejects_without_token() {
    assert_eq!(
        get_status(matchit_policy_app(false), "/protected", None).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn matchit_policy_protected_route_accepts_valid_token() {
    let bearer = format!("Bearer {GOOD_TOKEN}");
    assert_eq!(
        get_status(matchit_policy_app(false), "/protected", Some(&bearer)).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn matchit_policy_unmatched_route_follows_require_auth_by_default() {
    // require_auth_by_default = true → unmatched `/other` needs a token.
    assert_eq!(
        get_status(matchit_policy_app(true), "/other", None).await,
        StatusCode::UNAUTHORIZED
    );
    // require_auth_by_default = false → unmatched `/other` passes anonymously.
    assert_eq!(
        get_status(matchit_policy_app(false), "/other", None).await,
        StatusCode::OK
    );
}
