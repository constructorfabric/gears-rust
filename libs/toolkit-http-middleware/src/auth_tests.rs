use super::*;

use axum::{
    Extension, Router,
    body::{Body, to_bytes},
    routing::get,
};
use http::{HeaderMap, Request as HttpRequest, StatusCode, header};
use std::sync::atomic::{AtomicUsize, Ordering};
use toolkit_security::{
    InternalAuthNError, InternalAuthenticator, PlatformIdentity, SecurityContext,
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
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        match token {
            GOOD_TOKEN => Ok(SecurityContext::anonymous()),
            UNAVAILABLE_TOKEN => Err(AuthNError::Unavailable),
            INTERNAL_TOKEN => Err(AuthNError::Other("boom".to_owned())),
            _ => Err(AuthNError::InvalidToken),
        }
    }
}

fn app(is_anonymous: bool) -> Router {
    let authenticator = Arc::new(StubAuthenticator);

    // `security_context_middleware` runs as a `route_layer` (after routing); the
    // `AnonymousRoute` marker is added as an outer router `layer` so it is
    // present in the request extensions by the time the middleware reads it
    // (this mirrors how the bootstrap layer surfaces `!OperationSpec.authenticated` per-route).
    let secctx = axum::middleware::from_fn_with_state(
        authenticator,
        security_context_middleware::<StubAuthenticator>,
    );

    let router = Router::new()
        .route("/", get(|| async { StatusCode::OK }))
        .route_layer(secctx);

    if is_anonymous {
        router.layer(axum::Extension(AnonymousRoute))
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

/// Handler reporting whether the [`PlatformAuthEnforced`] posture marker is
/// present (`"1"`) or absent (`"0"`) in the request extensions.
async fn marker_echo(marker: Option<Extension<PlatformAuthEnforced>>) -> String {
    if marker.is_some() { "1" } else { "0" }.to_owned()
}

fn marker_app() -> Router {
    let authenticator = Arc::new(StubInternalAuthenticator);
    let layer = axum::middleware::from_fn_with_state(
        authenticator,
        internal_auth_middleware::<StubInternalAuthenticator>,
    );
    Router::new()
        .route("/", get(marker_echo))
        .route_layer(layer)
}

/// Stacked app: `internal_auth_middleware` (outermost, runs first) then
/// `security_context_middleware`, mirroring the DESIGN § 3.2 middleware order.
fn stacked_app() -> Router {
    let bearer = Arc::new(StubAuthenticator);
    let internal = Arc::new(StubInternalAuthenticator);
    let secctx = axum::middleware::from_fn_with_state(
        bearer,
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
    let bearer = Arc::new(StubAuthenticator);
    let internal = Arc::new(StubInternalAuthenticator);
    let secctx = axum::middleware::from_fn_with_state(
        bearer,
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
        .layer(axum::Extension(AnonymousRoute))
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
async fn internal_stamps_posture_marker_for_authenticated_request() {
    let (status, _, body) = send_headers(marker_app(), &[(INTERNAL_HEADER, SA_GOOD)]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "1",
        "an enforcing HTTP listener must stamp the posture marker"
    );
}

#[tokio::test]
async fn internal_stamps_posture_marker_for_anonymous_passthrough() {
    // The inversion this closes: an anonymous caller on an enforcing HTTP
    // listener (no token, permissive pass-through) must still carry the marker
    // so a downstream handler can fail closed rather than treat it as a listener
    // with no platform plane.
    let (status, _, body) = send_headers(marker_app(), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "1",
        "anonymous pass-through on an enforcing listener must still carry the marker"
    );
}

#[tokio::test]
async fn no_posture_marker_without_the_enforcement_layer() {
    // Negative control for the two "1" cases above: with no
    // `internal_auth_middleware`, nothing stamps the marker and `marker_echo`
    // reports "0". Without this, an extractor/handler that always reported the
    // marker present would pass both positive tests.
    let app = Router::new().route("/", get(marker_echo));
    let (status, _, body) = send_headers(app, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, "0",
        "a listener without the enforcement layer must leave the marker absent"
    );
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
    // Valid SA token, no JWT, route is AnonymousRoute — the normal probe/platform path.
    let (status, _, _) = send_headers(stacked_public_app(), &[(INTERNAL_HEADER, SA_GOOD)]).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn public_endpoint_with_no_credentials_passes() {
    // No SA token and no JWT on an AnonymousRoute: passes (health probe).
    let (status, _, _) = send_headers(stacked_public_app(), &[]).await;
    assert_eq!(status, StatusCode::OK);
}

// Platform route admission (`RouteAuth::Platform`).

/// A bearer the stub resolves to a real (non-anonymous) tenant subject.
const TENANT_TOKEN: &str = "tenant-token";

/// Stub: `TENANT_TOKEN` is a tenant subject, `GOOD_TOKEN` anonymous; other tokens fail.
struct SubjectAuthenticator;

impl BearerAuthenticator for SubjectAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        match token {
            TENANT_TOKEN => Ok(SecurityContext::builder()
                .subject_id(uuid::Uuid::from_u128(1))
                .subject_tenant_id(uuid::Uuid::from_u128(2))
                .build()
                .unwrap()),
            GOOD_TOKEN => Ok(SecurityContext::anonymous()),
            _ => Err(AuthNError::InvalidToken),
        }
    }
}

/// Platform caller and presence of a validated tenant bearer.
async fn platform_echo(
    Extension(ctx): Extension<PlatformSecurityContext>,
    tenant: Option<Extension<SecurityContext>>,
) -> String {
    match (ctx.identity().peer_name(), tenant.is_some()) {
        (peer, false) => peer.to_owned(),
        (peer, true) => format!("{peer}+tenant"),
    }
}

/// Tenant subject on an anonymous route: `anonymous` or `subject`.
async fn anon_echo(Extension(ctx): Extension<SecurityContext>) -> &'static str {
    if ctx.is_anonymous() {
        "anonymous"
    } else {
        "subject"
    }
}

/// Tenant-side echo: whether the platform context and the internal token header reached it.
async fn tenant_echo(
    platform: Option<Extension<PlatformSecurityContext>>,
    headers: HeaderMap,
) -> String {
    format!(
        "platform={} token_header={}",
        platform.is_some(),
        headers.contains_key(INTERNAL_HEADER)
    )
}

/// Stack built by [`layer_route_auth`]; auth planes are optional.
/// `/p` is platform, `/anon` anonymous, `/auth` authenticated, `/unlisted` unspecified.
fn platform_route_app(bearer: bool, internal: bool) -> Router {
    counted_route_app(bearer, internal).0
}

/// Calls each authenticator has received.
#[derive(Clone, Default)]
struct Calls {
    bearer: Arc<AtomicUsize>,
    internal: Arc<AtomicUsize>,
}

impl Calls {
    fn get(&self) -> (usize, usize) {
        (
            self.bearer.load(Ordering::SeqCst),
            self.internal.load(Ordering::SeqCst),
        )
    }
}

/// [`SubjectAuthenticator`] counting its calls.
struct CountedBearer(Arc<AtomicUsize>);

impl BearerAuthenticator for CountedBearer {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        SubjectAuthenticator.authenticate(token).await
    }
}

/// [`StubInternalAuthenticator`] counting its calls.
struct CountedInternal(Arc<AtomicUsize>);

impl InternalAuthenticator for CountedInternal {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        StubInternalAuthenticator.authenticate(token).await
    }
}

/// [`platform_route_app`] with the calls its authenticators receive.
fn counted_route_app(bearer: bool, internal: bool) -> (Router, Calls) {
    let calls = Calls::default();
    let policy: RouteAuthPolicy = [
        (Method::GET, "/p".to_owned(), RouteAuth::Platform),
        (Method::POST, "/p".to_owned(), RouteAuth::Platform),
        (Method::GET, "/anon".to_owned(), RouteAuth::Anonymous),
        (Method::GET, "/auth".to_owned(), RouteAuth::Authenticated),
    ]
    .into_iter()
    .collect();

    let router = Router::new()
        .route("/p", get(platform_echo).post(platform_echo))
        .route("/anon", get(anon_echo))
        .route("/auth", get(tenant_echo))
        .route("/unlisted", get(tenant_echo));
    let router = layer_route_auth(
        router,
        policy,
        bearer.then(|| DynBearerAuthenticator::new(CountedBearer(calls.bearer.clone()))),
        internal.then(|| DynInternalAuthenticator::new(CountedInternal(calls.internal.clone()))),
    );
    (router, calls)
}

const BEARER_TENANT: &str = "Bearer tenant-token";
const BEARER_FORGED: &str = "Bearer forged-token";

/// Request with optional credentials; returns status, content type, and body.
async fn call(
    router: Router,
    method: Method,
    path: &str,
    bearer: Option<&str>,
    token: Option<&str>,
) -> (StatusCode, Option<String>, String) {
    let mut builder = HttpRequest::builder().method(method).uri(path);
    if let Some(value) = bearer {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    if let Some(value) = token {
        builder = builder.header(INTERNAL_HEADER, value);
    }
    let response = router
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
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

fn assert_401(result: &(StatusCode, Option<String>, String), case: &str) {
    assert_eq!(result.0, StatusCode::UNAUTHORIZED, "{case}: {}", result.2);
    assert_eq!(result.1.as_deref(), Some(PROBLEM_JSON), "{case}");
}

fn assert_reason(result: &(StatusCode, Option<String>, String), reason: &str, case: &str) {
    assert_401(result, case);
    assert!(result.2.contains(reason), "{case}: {}", result.2);
}

#[tokio::test]
async fn a_platform_route_admits_a_validated_internal_token() {
    for method in [Method::GET, Method::POST] {
        let (status, _, body) = call(
            platform_route_app(true, true),
            method.clone(),
            "/p",
            None,
            Some(SA_GOOD),
        )
        .await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "flight-control"),
            "{method}"
        );
    }
}

#[tokio::test]
async fn a_platform_route_refuses_a_bearer_alone_and_an_invalid_token() {
    for (bearer, token, reason, case) in [
        (None, None, "MISSING_INTERNAL_TOKEN", "neither"),
        (
            Some(BEARER_TENANT),
            None,
            "MISSING_INTERNAL_TOKEN",
            "valid bearer alone",
        ),
        (
            Some(BEARER_FORGED),
            None,
            "MISSING_INTERNAL_TOKEN",
            "forged bearer alone",
        ),
        (None, Some("forged"), "INTERNAL_AUTH_FAILED", "forged token"),
    ] {
        let result = call(
            platform_route_app(true, true),
            Method::POST,
            "/p",
            bearer,
            token,
        )
        .await;
        assert_reason(&result, reason, case);
    }
}

/// Both planes without the inner gate, as a hand-wired stack might be built.
fn platform_app_without_gate() -> Router {
    let policy: RouteAuthPolicy = [(Method::GET, "/p".to_owned(), RouteAuth::Platform)]
        .into_iter()
        .collect();
    Router::new()
        .route("/p", get(platform_echo))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(DynBearerAuthenticator::new(SubjectAuthenticator)),
            security_context_middleware::<DynBearerAuthenticator>,
        ))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(DynInternalAuthenticator::new(StubInternalAuthenticator)),
            internal_auth_middleware::<DynInternalAuthenticator>,
        ))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(policy),
            route_auth_middleware,
        ))
}

#[tokio::test]
async fn the_tenant_plane_admits_platform_routes_without_the_gate() {
    for (bearer, case) in [
        (None, "neither"),
        (Some(BEARER_TENANT), "valid bearer alone"),
        (Some(BEARER_FORGED), "forged bearer alone"),
    ] {
        let result = call(platform_app_without_gate(), Method::GET, "/p", bearer, None).await;
        assert_reason(&result, "MISSING_INTERNAL_TOKEN", case);
    }
    let (status, _, body) = call(
        platform_app_without_gate(),
        Method::GET,
        "/p",
        None,
        Some(SA_GOOD),
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "flight-control"));
}

#[tokio::test]
async fn head_inherits_the_get_policy() {
    let (status, _, _) = call(
        platform_route_app(true, true),
        Method::HEAD,
        "/p",
        None,
        Some(SA_GOOD),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let result = call(
        platform_route_app(true, true),
        Method::HEAD,
        "/p",
        Some(BEARER_TENANT),
        None,
    )
    .await;
    assert_eq!(result.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn without_a_tenant_plane_a_platform_route_admits_only_a_token() {
    let (status, _, body) = call(
        platform_route_app(false, true),
        Method::GET,
        "/p",
        None,
        Some(SA_GOOD),
    )
    .await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "flight-control"));
    let result = call(
        platform_route_app(false, true),
        Method::GET,
        "/p",
        Some(BEARER_TENANT),
        None,
    )
    .await;
    assert_reason(&result, "MISSING_INTERNAL_TOKEN", "bearer alone");
}

#[tokio::test]
async fn without_a_platform_plane_every_platform_request_is_refused() {
    for (bearer, token, reason, case) in [
        (
            None,
            Some(SA_GOOD),
            "UNVERIFIED_INTERNAL_TOKEN",
            "token alone",
        ),
        (None, None, "MISSING_INTERNAL_TOKEN", "neither"),
    ] {
        let result = call(
            platform_route_app(true, false),
            Method::GET,
            "/p",
            bearer,
            token,
        )
        .await;
        assert_reason(&result, reason, case);
    }
}

#[tokio::test]
async fn both_credentials_are_refused_before_either_authenticator() {
    for path in ["/p", "/auth", "/anon", "/unlisted"] {
        for authorization in [
            BEARER_TENANT,
            BEARER_FORGED,
            "Basic dXNlcjpwYXNz",
            "Bearer ",
        ] {
            for token in [SA_GOOD, "forged"] {
                for (bearer_plane, internal_plane) in
                    [(true, true), (true, false), (false, true), (false, false)]
                {
                    let (app, calls) = counted_route_app(bearer_plane, internal_plane);
                    let case = format!(
                        "{path} {authorization:?} token={token} \
                         planes=({bearer_plane}, {internal_plane})"
                    );
                    let result =
                        call(app, Method::GET, path, Some(authorization), Some(token)).await;
                    assert_reason(&result, MixedCredentials::REASON, &case);
                    assert_eq!(calls.get(), (0, 0), "{case}");
                }
            }
        }
    }
}

#[tokio::test]
async fn a_lone_credential_of_the_other_plane_is_dropped_unvalidated() {
    let (app, calls) = counted_route_app(true, true);
    let result = call(app, Method::GET, "/p", Some(BEARER_FORGED), None).await;
    assert_reason(
        &result,
        "MISSING_INTERNAL_TOKEN",
        "forged bearer on a platform route",
    );
    assert_eq!(calls.get(), (0, 0));
    for path in ["/auth", "/unlisted"] {
        let (app, calls) = counted_route_app(true, true);
        let result = call(app, Method::GET, path, None, Some("forged")).await;
        assert_reason(&result, "MISSING_BEARER", path);
        assert_eq!(calls.get(), (0, 0), "{path}");
        // Without a tenant plane the handler runs, without the token or a platform context.
        let (app, calls) = counted_route_app(false, true);
        let (status, _, body) = call(app, Method::GET, path, None, Some(SA_GOOD)).await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "platform=false token_header=false"),
            "{path}"
        );
        assert_eq!(calls.get(), (0, 0), "{path}");
    }
    // Anonymous routes keep validating a lone credential of either plane.
    let (app, calls) = counted_route_app(true, true);
    let result = call(app, Method::GET, "/anon", None, Some("forged")).await;
    assert_401(&result, "forged token on an anonymous route");
    assert_eq!(calls.get(), (0, 1));
}

#[tokio::test]
async fn a_misordered_policy_still_drops_the_other_planes_context() {
    // The policy inside the platform plane: the token is validated before the policy drops it.
    let policy: RouteAuthPolicy = [(Method::GET, "/auth".to_owned(), RouteAuth::Authenticated)]
        .into_iter()
        .collect();
    let app = Router::new()
        .route("/auth", get(tenant_echo))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(policy),
            route_auth_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(DynInternalAuthenticator::new(StubInternalAuthenticator)),
            internal_auth_middleware::<DynInternalAuthenticator>,
        ));
    let (status, _, body) = call(app, Method::GET, "/auth", None, Some(SA_GOOD)).await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::OK, "platform=false token_header=false")
    );
}

/// Injects a sentinel context before the gate; the handler itself reads no context.
fn sentinel_app<T: Clone + Send + Sync + 'static>(sentinel: T) -> Router {
    let policy: RouteAuthPolicy = [(Method::GET, "/p".to_owned(), RouteAuth::Platform)]
        .into_iter()
        .collect();
    Router::new()
        .route("/p", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(platform_route_middleware))
        .layer(Extension(sentinel))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(policy),
            route_auth_middleware,
        ))
}

#[tokio::test]
async fn the_gate_does_not_admit_the_outbound_marker() {
    // The locally minted outbound marker is not a validated platform caller.
    let result = call(
        sentinel_app(PlatformSecurityContext::outbound_marker()),
        Method::GET,
        "/p",
        None,
        None,
    )
    .await;
    assert_reason(&result, "MISSING_INTERNAL_TOKEN", "outbound marker");
}

#[tokio::test]
async fn platform_admission_refuses_both_credentials_without_the_policy_layer() {
    // `RouteAuth::Platform` set by hand, so `retain_route_plane` never runs: the token is
    // validated, and admission alone must still refuse the pair.
    let internal = || {
        axum::middleware::from_fn_with_state(
            Arc::new(DynInternalAuthenticator::new(StubInternalAuthenticator)),
            internal_auth_middleware::<DynInternalAuthenticator>,
        )
    };
    let gate_only = Router::new()
        .route("/p", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn(platform_route_middleware))
        .layer(internal())
        .layer(Extension(RouteAuth::Platform));
    let tenant_plane_only = Router::new()
        .route("/p", get(|| async { "ok" }))
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(DynBearerAuthenticator::new(SubjectAuthenticator)),
            security_context_middleware::<DynBearerAuthenticator>,
        ))
        .layer(internal())
        .layer(Extension(RouteAuth::Platform));
    for (app, case) in [(gate_only, "gate"), (tenant_plane_only, "tenant plane")] {
        let result = call(app, Method::GET, "/p", Some(BEARER_TENANT), Some(SA_GOOD)).await;
        assert_reason(&result, MixedCredentials::REASON, case);
    }
}

#[tokio::test]
async fn anonymous_routes_get_a_security_context_with_or_without_a_tenant_plane() {
    for bearer_plane in [true, false] {
        let (status, _, body) = call(
            platform_route_app(bearer_plane, true),
            Method::GET,
            "/anon",
            None,
            None,
        )
        .await;
        assert_eq!(
            (status, body.as_str()),
            (StatusCode::OK, "anonymous"),
            "no credentials (tenant plane {bearer_plane})"
        );
    }
    let (status, _, body) = call(
        platform_route_app(true, true),
        Method::GET,
        "/anon",
        Some(BEARER_TENANT),
        None,
    )
    .await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::OK, "subject"),
        "a validated bearer replaces the anonymous context"
    );
    let result = call(
        platform_route_app(true, true),
        Method::GET,
        "/anon",
        Some(BEARER_FORGED),
        None,
    )
    .await;
    assert_401(&result, "a presented bearer is still validated");
}

#[tokio::test]
async fn the_policy_reaches_anonymous_and_authenticated_routes() {
    let result = call(
        platform_route_app(true, true),
        Method::GET,
        "/auth",
        None,
        Some(SA_GOOD),
    )
    .await;
    assert_401(&result, "authenticated route on an internal token alone");
    let result = call(
        platform_route_app(true, true),
        Method::GET,
        "/unlisted",
        None,
        None,
    )
    .await;
    assert_401(
        &result,
        "a route without a policy entry keeps requiring a bearer",
    );
}

#[test]
fn head_resolves_as_get_unless_it_has_its_own_entry() {
    let mut policy = RouteAuthPolicy::default();
    policy.insert(Method::GET, "/a", RouteAuth::Platform);
    policy.insert(Method::GET, "/b", RouteAuth::Platform);
    policy.insert(Method::HEAD, "/b", RouteAuth::Anonymous);
    assert_eq!(
        policy.resolve(&Method::HEAD, "/a"),
        Some(RouteAuth::Platform)
    );
    assert_eq!(
        policy.resolve(&Method::HEAD, "/b"),
        Some(RouteAuth::Anonymous)
    );
    assert_eq!(policy.resolve(&Method::POST, "/a"), None);
    assert_eq!(
        policy.resolve(&Method::GET, "/a/x"),
        None,
        "exact templates only"
    );
}
