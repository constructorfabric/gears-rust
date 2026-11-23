//! Out-of-process (`OoP`) HTTP server: probes, two-plane auth, and graceful
//! drain (`cpt-cf-component-oop-bootstrap`).
//!
//! This module owns the edge of an `OoP` gear's HTTP surface:
//!
//! - **Framework probes** — `/healthz` (liveness), `/readyz` (readiness, gated
//!   on dependency resolution + custom checks), `/health` (diagnostics, full
//!   healthcheck report), and `/.well-known/openapi.json` (the canonical
//!   discovery path; `cpt-cf-binding-constraint-openapi-well-known`).
//! - **Two-plane auth** — platform-plane ([`internal_auth_middleware`]) runs
//!   *before* tenant-plane ([`security_context_middleware`]) per
//!   `cpt-cf-adr-two-plane-auth`,
//!   but only when the corresponding authenticator is injected. Because both
//!   authenticator traits use return-position `impl Trait` (not `dyn`-safe),
//!   callers inject the object-safe [`DynBearerAuthenticator`] /
//!   [`DynInternalAuthenticator`] adapters.
//! - **Graceful drain** — a drain guard rejects new gear-route requests with
//!   `503 + Retry-After` once draining begins, while in-flight requests finish.
//! - **Framework middleware** — a canonical-error layer normalizes gear/auth
//!   error responses into `problem+json` (`trace_id` / `instance` filled),
//!   matching the in-process (`api-gateway`) path. The drain guard also catches
//!   panics so a panic can neither drop the client connection nor leak the
//!   in-flight counter (which would otherwise stall graceful drain).
//! - **Request/trace context** — the outermost `trace_context_middleware`
//!   continues the inbound W3C `traceparent` and scopes the inbound
//!   `x-request-id` (`toolkit_trace_context::with_request_id`), so the gear's
//!   span carries the edge id and outbound `toolkit-http` calls forward it.
//!
//! The server is driven by [`OopHttpServer`], invoked from the `HostRuntime`
//! `OoP` path: it binds and serves the probes **before** the gear's `start()`
//! phase (so `/healthz` is up during a slow startup), then swaps in the composed
//! gear routes. Self-registration and dependency resolution live in
//! [`super::oop_registration`].

use std::collections::{BTreeMap, HashMap};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, MatchedPath, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header::RETRY_AFTER},
    middleware::{Next, from_fn, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::FutureExt as _;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;
use tower_http::limit::RequestBodyLimitLayer;
use url::Url;

use cf_system_sdks::directory::{DirectoryClient, RegisterInstanceInfo, ServiceEndpoint};
use toolkit_canonical_errors::{CanonicalError, resource_error};
use toolkit_http_middleware::{RouteAuthPolicy, TenantPlane, layer_route_auth_with};
use toolkit_security::{DynBearerAuthenticator, DynInternalAuthenticator};
use toolkit_trace_context::{TRACEPARENT, parse_trace_id};

use super::presence_manager::{PresenceManager, ReportedStateSource};
use super::readiness::ReadinessState;
use crate::api::canonical_error_middleware;
use crate::api::operation_builder::{OperationSpec, RouteTimeout};

/// `Retry-After` (seconds) advertised while the gear is draining.
const DRAIN_RETRY_AFTER_SECONDS: u64 = 5;

/// `Retry-After` (seconds) advertised for gear routes before the gear has
/// finished starting (probes are already live; routes come up shortly).
const STARTING_RETRY_AFTER_SECONDS: u64 = 1;

// ---------------------------------------------------------------------------
// Serve options
// ---------------------------------------------------------------------------

/// Request body and response deadline policy for published `OoP` gear routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "RawOopRequestLimits")]
pub struct OopRequestLimits {
    body_limit_bytes: usize,
    #[serde(with = "toolkit_utils::humantime_serde")]
    request_timeout: Duration,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOopRequestLimits {
    body_limit_bytes: usize,
    #[serde(with = "toolkit_utils::humantime_serde")]
    request_timeout: Duration,
}

impl TryFrom<RawOopRequestLimits> for OopRequestLimits {
    type Error = OopRequestLimitsError;

    fn try_from(raw: RawOopRequestLimits) -> Result<Self, Self::Error> {
        Self::new(raw.body_limit_bytes, raw.request_timeout)
    }
}

/// An invalid request limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OopRequestLimitsError {
    /// The body byte limit is zero.
    #[error("body_limit_bytes must be positive")]
    BodyLimitZero,
    /// The response deadline is zero.
    #[error("request_timeout must be positive")]
    RequestTimeoutZero,
}

impl OopRequestLimits {
    /// Create a policy with positive body bytes and a positive response deadline.
    ///
    /// # Errors
    ///
    /// Returns [`OopRequestLimitsError::BodyLimitZero`] for a zero byte limit or
    /// [`OopRequestLimitsError::RequestTimeoutZero`] for a zero timeout.
    pub fn new(
        body_limit_bytes: usize,
        request_timeout: Duration,
    ) -> Result<Self, OopRequestLimitsError> {
        if body_limit_bytes == 0 {
            return Err(OopRequestLimitsError::BodyLimitZero);
        }
        if request_timeout.is_zero() {
            return Err(OopRequestLimitsError::RequestTimeoutZero);
        }
        Ok(Self {
            body_limit_bytes,
            request_timeout,
        })
    }

    /// Maximum number of request body bytes.
    #[must_use]
    pub const fn body_limit_bytes(&self) -> usize {
        self.body_limit_bytes
    }

    /// Default time to produce a response. A registered operation may replace or
    /// disable this deadline. Response streams continue independently.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }
}

/// Configuration and collaborators for the `OoP` HTTP runtime, assembled by the
/// bootstrap layer and consumed by `HostRuntime`'s `OoP` serving path.
///
/// `#[non_exhaustive]` so new fields (like `labels`) stay additive. Construct via
/// [`OopServeOptions::new`] + the `with_*` builders rather than a struct literal,
/// exactly as [`RunOptions`](super::RunOptions) is built: the five arguments to
/// `new` are the fields with no meaningful default, and everything else defaults
/// to the same values the bootstrap config layer uses. (A `Default` impl is still
/// not provided — the `directory` collaborator is a required dependency.)
///
/// The builders are what keep [`run_oop_serving`](super::run_oop_serving)
/// callable: it is `pub` and takes this type by value, so an out-of-crate caller
/// — an integration test driving the real serve path, for instance — needs some
/// way to build one.
#[non_exhaustive]
pub struct OopServeOptions {
    /// Logical gear name (used for registration + `OpenAPI` title).
    pub gear_name: String,
    /// Process instance id (used for registration/deregistration).
    pub instance_id: String,
    /// Optional gear version (used for registration + `OpenAPI` version).
    pub version: Option<String>,
    /// Base URL other services use to reach this instance's REST endpoint
    /// (e.g. `http://billing.default.svc.cluster.local:8080`). Registered with
    /// `DirectoryService` as the instance's `rest_endpoint`.
    pub advertise_uri: String,
    /// Address the main HTTP server binds to (gear routes + probes).
    pub listen_addr: std::net::SocketAddr,
    /// Optional separate address for probe endpoints (sidecar port). When set,
    /// probes are served here *and* on the main listener.
    pub probe_bind_addr: Option<std::net::SocketAddr>,
    /// Maximum time to wait for in-flight requests to drain on shutdown.
    pub drain_timeout: Duration,
    /// Fallback heartbeat interval, used only when the directory grants no lease
    /// TTL (an older directory). Otherwise the [`PresenceManager`] renews at
    /// `lease_ttl / 3`. Clamped to a 1s minimum so it can't hot-loop.
    pub heartbeat_interval: Duration,
    /// Per-check timeout for readiness healthchecks (`/readyz`). A gear's
    /// [`Healthcheck`](crate::healthcheck::Healthcheck) that exceeds this is
    /// reported `Unhealthy`. Mirrors the `api-gateway` `healthcheck_timeout_ms`.
    pub healthcheck_timeout: Duration,
    /// Optional body and response deadline policy for published gear routes.
    pub request_limits: Option<OopRequestLimits>,
    /// Directory client used for self-registration and dependency resolution.
    pub directory: Arc<dyn DirectoryClient>,
    /// Tenant-plane authenticator; when `Some`, `security_context_middleware`
    /// is installed on gear routes.
    pub bearer_authenticator: Option<DynBearerAuthenticator>,
    /// Platform-plane authenticator; when `Some`, `internal_auth_middleware` is
    /// installed on gear routes (runs before the tenant plane).
    pub internal_authenticator: Option<DynInternalAuthenticator>,
    /// Development only (`oop_http.auth_disabled`): without a tenant authenticator,
    /// non-platform routes run with the default tenant `SecurityContext` instead of
    /// being refused. Platform routes still require a validated platform caller.
    pub auth_disabled: bool,
    /// Stable addressing labels (k8s `matchLabels` style) advertised with this
    /// instance's directory registration for label-based selection.
    pub labels: BTreeMap<String, String>,
}

impl OopServeOptions {
    /// Create `OopServeOptions` with the fields that have no meaningful default;
    /// every optional field defaults to the bootstrap config layer's own default
    /// (`drain_timeout` 30s, `heartbeat_interval` 5s, `healthcheck_timeout`
    /// 500ms). Add optional request limits, probe sidecar, authenticators, and labels
    /// with the `with_*` builders.
    #[must_use]
    pub fn new(
        gear_name: String,
        instance_id: String,
        advertise_uri: String,
        listen_addr: std::net::SocketAddr,
        directory: Arc<dyn DirectoryClient>,
    ) -> Self {
        Self {
            gear_name,
            instance_id,
            version: None,
            advertise_uri,
            listen_addr,
            probe_bind_addr: None,
            drain_timeout: Duration::from_secs(30),
            heartbeat_interval: Duration::from_secs(5),
            healthcheck_timeout: Duration::from_millis(500),
            request_limits: None,
            directory,
            bearer_authenticator: None,
            internal_authenticator: None,
            auth_disabled: false,
            labels: BTreeMap::new(),
        }
    }

    /// Development only: see [`Self::auth_disabled`].
    #[must_use]
    pub fn with_auth_disabled(mut self, auth_disabled: bool) -> Self {
        self.auth_disabled = auth_disabled;
        self
    }

    /// The listener's tenant plane: the bearer authenticator when one is installed, else
    /// [`TenantPlane::Disabled`] under `auth_disabled`, else [`TenantPlane::Absent`].
    #[must_use]
    pub fn tenant_plane(&self) -> TenantPlane {
        match (&self.bearer_authenticator, self.auth_disabled) {
            (Some(bearer), _) => TenantPlane::Authenticator(bearer.clone()),
            (None, true) => TenantPlane::Disabled,
            (None, false) => TenantPlane::Absent,
        }
    }

    /// Set the gear version (used for registration + `OpenAPI` version).
    #[must_use]
    pub fn with_version(mut self, version: Option<String>) -> Self {
        self.version = version;
        self
    }

    /// Serve probes on a separate sidecar port *in addition to* the main listener.
    #[must_use]
    pub fn with_probe_bind_addr(mut self, addr: Option<std::net::SocketAddr>) -> Self {
        self.probe_bind_addr = addr;
        self
    }

    /// Override how long in-flight requests may drain on shutdown.
    #[must_use]
    pub fn with_drain_timeout(mut self, timeout: Duration) -> Self {
        self.drain_timeout = timeout;
        self
    }

    /// Override the `DirectoryService` heartbeat interval.
    #[must_use]
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    /// Override the per-check readiness healthcheck timeout.
    #[must_use]
    pub fn with_healthcheck_timeout(mut self, timeout: Duration) -> Self {
        self.healthcheck_timeout = timeout;
        self
    }

    /// Apply request limits to published gear routes.
    #[must_use]
    pub fn with_request_limits(mut self, limits: OopRequestLimits) -> Self {
        self.request_limits = Some(limits);
        self
    }

    /// Install the tenant-plane authenticator (`security_context_middleware`).
    #[must_use]
    pub fn with_bearer_authenticator(mut self, auth: Option<DynBearerAuthenticator>) -> Self {
        self.bearer_authenticator = auth;
        self
    }

    /// Install the platform-plane authenticator (`internal_auth_middleware`).
    #[must_use]
    pub fn with_internal_authenticator(mut self, auth: Option<DynInternalAuthenticator>) -> Self {
        self.internal_authenticator = auth;
        self
    }

    /// Advertise stable addressing labels for label-based selection.
    #[must_use]
    pub fn with_labels(mut self, labels: BTreeMap<String, String>) -> Self {
        self.labels = labels;
        self
    }
}

impl std::fmt::Debug for OopServeOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OopServeOptions")
            .field("gear_name", &self.gear_name)
            .field("instance_id", &self.instance_id)
            .field("version", &self.version)
            .field("advertise_uri", &self.advertise_uri)
            .field("listen_addr", &self.listen_addr)
            .field("probe_bind_addr", &self.probe_bind_addr)
            .field("drain_timeout", &self.drain_timeout)
            .field("heartbeat_interval", &self.heartbeat_interval)
            .field("healthcheck_timeout", &self.healthcheck_timeout)
            .field("request_limits", &self.request_limits)
            .field("bearer_authenticator", &self.bearer_authenticator.is_some())
            .field(
                "internal_authenticator",
                &self.internal_authenticator.is_some(),
            )
            .field("labels", &self.labels)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Probe router
// ---------------------------------------------------------------------------

/// Composed gear routes + `OpenAPI` document, published *after* the gear's
/// `start()` phase completes.
///
/// Held behind an [`arc_swap::ArcSwapOption`] so the probe listener can bind and
/// serve `/healthz` immediately (while a slow `start()` runs), then have the gear
/// routes swapped in atomically — no port rebind, so liveness never blips.
#[derive(Clone, Default)]
struct LateRoutes {
    inner: Arc<arc_swap::ArcSwapOption<LateRoutesInner>>,
}

struct LateRoutesInner {
    /// Fully-layered gear router (auth planes + drain guard + canonical errors).
    gear: Router,
    /// Serialized `OpenAPI` document served at the well-known path.
    openapi: Arc<str>,
}

impl LateRoutes {
    /// Publish the composed gear routes; subsequent requests are served by them.
    fn publish(&self, gear: Router, openapi: Arc<str>) {
        self.inner
            .store(Some(Arc::new(LateRoutesInner { gear, openapi })));
    }

    /// The current gear router, if the gear has finished starting.
    fn gear(&self) -> Option<Router> {
        self.inner.load_full().map(|i| i.gear.clone())
    }

    /// The current `OpenAPI` document, if published.
    fn openapi(&self) -> Option<Arc<str>> {
        self.inner.load_full().map(|i| Arc::clone(&i.openapi))
    }
}

/// `503 + Retry-After` returned for gear routes before the gear has started.
fn starting_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(RETRY_AFTER, STARTING_RETRY_AFTER_SECONDS.to_string())],
        "starting",
    )
        .into_response()
}

/// Shared state for the probe endpoints.
#[derive(Clone)]
struct ProbeState {
    readiness: Arc<ReadinessState>,
    late: LateRoutes,
}

/// Build the outer router bound at startup: framework probes plus a gear-route
/// fallback that is `503 starting` until [`LateRoutes::publish`] swaps in the
/// composed gear router. Probes are live as soon as the listener binds.
fn build_outer_router(readiness: Arc<ReadinessState>, late: LateRoutes) -> Router {
    let fallback_late = late.clone();
    build_probe_router(readiness, late).fallback_service(tower::util::service_fn(
        move |req: Request| {
            let late = fallback_late.clone();
            async move {
                match late.gear() {
                    // `Router`'s service error is `Infallible`, hence `match e {}`.
                    Some(router) => Ok::<_, std::convert::Infallible>(
                        router.oneshot(req).await.unwrap_or_else(|e| match e {}),
                    ),
                    None => Ok::<_, std::convert::Infallible>(starting_response()),
                }
            }
        },
    ))
}

/// Build the framework probe router: `/healthz`, `/readyz`, `/health`, and the
/// well-known `OpenAPI` discovery path.
///
/// These routes carry no tenant JWT and are never subject to the drain guard or
/// the auth middlewares (they must respond during startup and drain).
fn build_probe_router(readiness: Arc<ReadinessState>, late: LateRoutes) -> Router {
    let state = ProbeState { readiness, late };

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/health", get(health))
        .route("/.well-known/openapi.json", get(openapi))
        .route("/openapi.json", get(openapi))
        .with_state(state)
}

/// Liveness: always `200` with plain body `ok` once the server is listening.
async fn healthz() -> &'static str {
    "ok"
}

/// Readiness: `200` when ready, `503` with the unresolved-deps / failing-checks
/// body otherwise. `Degraded` checks keep the response `200`.
async fn readyz(State(state): State<ProbeState>) -> Response {
    let report = state.readiness.evaluate().await;
    let status = if report.ready {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report)).into_response()
}

/// Health: returns the full `HealthcheckReport` (`status` + per-component
/// detail). `200` for `Healthy`/`Degraded`, `503` for `Unhealthy`.
async fn health(State(state): State<ProbeState>) -> impl IntoResponse {
    let report = state.readiness.health_report().await;
    let status = if report.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(report))
}

/// Serve the gear's generated `OpenAPI` document once published (after
/// `start()`); `503 starting` beforehand.
async fn openapi(State(state): State<ProbeState>) -> Response {
    match state.late.openapi() {
        Some(spec) => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            spec.to_string(),
        )
            .into_response(),
        None => starting_response(),
    }
}

// ---------------------------------------------------------------------------
// Drain guard
// ---------------------------------------------------------------------------

/// Tracks in-flight gear-route requests and enforces the drain state.
#[derive(Clone)]
struct DrainGuard {
    readiness: Arc<ReadinessState>,
    in_flight: Arc<AtomicUsize>,
}

impl DrainGuard {
    fn new(readiness: Arc<ReadinessState>) -> Self {
        Self {
            readiness,
            in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Current number of in-flight gear-route requests.
    fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::SeqCst)
    }

    /// Begin draining: flip readiness to `503` so upstreams stop routing new
    /// traffic and the guard starts rejecting new requests (drain step 1/2).
    fn begin_drain(&self) {
        self.readiness.set_draining(true);
    }
}

/// RAII guard that increments the in-flight request counter on creation and
/// decrements it on drop, so cancellation or panic cannot leak a count.
struct InFlightGuard(Arc<AtomicUsize>);

impl InFlightGuard {
    fn new(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::SeqCst);
        Self(counter)
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Middleware: reject new requests with `503 + Retry-After` while draining;
/// otherwise track the request as in-flight until it completes.
async fn drain_guard_middleware(
    State(guard): State<DrainGuard>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    if guard.readiness.is_draining() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(RETRY_AFTER, DRAIN_RETRY_AFTER_SECONDS.to_string())],
            "draining",
        )
            .into_response();
    }

    // The in-flight counter is tracked by an RAII guard so it is decremented
    // whether the handler completes normally, panics, or the future is cancelled.
    // Panics are caught and converted to a canonical 500 (enriched by the outer
    // canonical-error layer) instead of dropping the connection — the OoP
    // equivalent of `tower_http`'s `CatchPanicLayer`.
    let _in_flight = InFlightGuard::new(guard.in_flight.clone());
    let outcome = AssertUnwindSafe(next.run(request)).catch_unwind().await;

    match outcome {
        Ok(response) => response,
        Err(panic) => {
            let detail = panic_message(panic.as_ref());
            tracing::error!(panic = %detail, "OoP gear handler panicked");
            // `detail` is a server-side diagnostic only; `CanonicalError::internal`
            // keeps it off the wire (`#[serde(skip)]`) per the error contract.
            CanonicalError::internal(format!("gear handler panicked: {detail}"))
                .create()
                .into_response()
        }
    }
}

/// Extract a human-readable message from a caught panic payload.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".to_owned())
}

// ---------------------------------------------------------------------------
// Router assembly
// ---------------------------------------------------------------------------

/// Before the application group started, serve only platform-plane and
/// anonymous routes: a tenant-plane route (and an unknown path) replies `503`
/// rather than run without a tenant authenticator.
async fn bootstrap_plane_gate(
    State(policy): State<Arc<RouteAuthPolicy>>,
    request: Request,
    next: Next,
) -> Response {
    let auth = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .and_then(|path| policy.resolve(request.method(), path.as_str()));
    match auth {
        Some(toolkit_security::RouteAuth::Platform | toolkit_security::RouteAuth::Anonymous) => {
            next.run(request).await
        }
        _ => CanonicalError::service_unavailable()
            .with_retry_after_seconds(STARTING_RETRY_AFTER_SECONDS)
            .with_detail("tenant-plane routes are served once the application group has started")
            .create()
            .into_response(),
    }
}

type RouteTimeouts = HashMap<Method, HashMap<String, RouteTimeout>>;

/// Operation policies for one publication of the gear router.
#[derive(Debug, Default)]
pub(super) struct OopRoutePolicy {
    auth: RouteAuthPolicy,
    timeouts: RouteTimeouts,
}

impl OopRoutePolicy {
    pub(super) fn from_specs<S>(specs: impl IntoIterator<Item = S>) -> Self
    where
        S: std::ops::Deref<Target = OperationSpec>,
    {
        let mut policy = Self::default();
        for spec in specs {
            policy
                .auth
                .insert(spec.method.clone(), spec.path.clone(), spec.auth);
            if spec.request_timeout != RouteTimeout::Inherit {
                policy
                    .timeouts
                    .entry(spec.method.clone())
                    .or_default()
                    .insert(spec.path.clone(), spec.request_timeout);
            }
        }
        policy
    }
}

/// Probes stay on [`build_outer_router`] so they remain available during startup and drain.
fn layer_gear_router(
    gear_router: Router,
    policy: OopRoutePolicy,
    tenant_plane: TenantPlane,
    drain_guard: DrainGuard,
    options: &OopServeOptions,
) -> Router {
    // Policy, both auth planes and the platform gate, in the order
    // `cpt-cf-adr-two-plane-auth` requires. Both planes fail closed: platform routes
    // without an internal authenticator, tenant and unlisted routes without a bearer
    // authenticator (unless `auth_disabled`). `check_route_auth` refuses to start
    // before that can happen for a declared tenant operation.
    let gear = layer_route_auth_with(
        gear_router,
        policy.auth,
        tenant_plane,
        options.internal_authenticator.clone(),
    );
    layer_serving(gear, drain_guard, options, policy.timeouts)
}

/// Assemble the gear ingress layers shared by early and final route publication.
fn layer_serving(
    gear: Router,
    drain_guard: DrainGuard,
    options: &OopServeOptions,
    timeouts: RouteTimeouts,
) -> Router {
    let drain = from_fn_with_state(drain_guard, drain_guard_middleware);
    let gear = match options.request_limits {
        Some(limits) => {
            let body_limit = limits.body_limit_bytes();
            let deadlines = Arc::new(RequestDeadlines {
                default: limits.request_timeout(),
                timeouts,
            });
            // Both layers use the same cap so opting in can raise Axum's extractor default.
            gear.layer(DefaultBodyLimit::max(body_limit))
                .layer(RequestBodyLimitLayer::new(body_limit))
                .layer(drain)
                .layer(from_fn_with_state(deadlines, RequestDeadlines::apply))
        }
        None => gear.layer(drain),
    };

    // Canonical errors use the outer request span to fill the wire trace ID.
    gear.layer(from_fn(canonical_error_middleware))
        .layer(from_fn(trace_context_middleware))
}

#[resource_error(gts_id!("cf.core.http.request.v1~"))]
struct RequestDeadlineError;

struct RequestDeadlines {
    default: Duration,
    timeouts: RouteTimeouts,
}

impl RequestDeadlines {
    fn resolve(&self, request: &Request) -> Option<Duration> {
        let declared = request
            .extensions()
            .get::<MatchedPath>()
            .and_then(|path| self.timeouts.get(request.method())?.get(path.as_str()));
        match declared {
            Some(RouteTimeout::Custom(timeout)) => Some(*timeout),
            Some(RouteTimeout::Disabled) => None,
            Some(RouteTimeout::Inherit) | None => Some(self.default),
        }
    }

    async fn apply(State(deadlines): State<Arc<Self>>, request: Request, next: Next) -> Response {
        let Some(timeout) = deadlines.resolve(&request) else {
            return next.run(request).await;
        };
        // Expiry drops the request future and releases its drain guard.
        match tokio::time::timeout(timeout, next.run(request)).await {
            Ok(response) => response,
            Err(_) => RequestDeadlineError::deadline_exceeded(format!(
                "Request exceeded {timeout:?} timeout"
            ))
            .create()
            .into_response(),
        }
    }
}

tokio::task_local! {
    static INBOUND_TRACEPARENT: Option<HeaderValue>;
}

/// Forward the current `OoP` request's single valid inbound `traceparent`
/// when the outbound headers lack a parent from the active span or caller.
pub fn inject_inbound_traceparent(headers: &mut HeaderMap) {
    if !headers.contains_key(TRACEPARENT)
        && let Ok(Some(parent)) = INBOUND_TRACEPARENT.try_with(Clone::clone)
    {
        headers.insert(TRACEPARENT, parent);
    }
}

/// Scope the caller's correlation headers to the gear request. Its span
/// records the wire trace ID even when no tracing provider is installed.
async fn trace_context_middleware(req: Request, next: Next) -> Response {
    use tracing::Instrument as _;
    use tracing::field::Empty;

    let mut parents = req.headers().get_all(TRACEPARENT).iter();
    let inbound = parents
        .next()
        .filter(|value| {
            parents.next().is_none()
                && value.len() == 55
                && value.to_str().ok().and_then(parse_trace_id).is_some()
        })
        .cloned();
    let request_id = toolkit_trace_context::extract_request_id(req.headers());

    let span = tracing::info_span!(
        "http_request",
        method = %req.method(),
        uri = %req.uri().path(),
        request_id = %request_id.as_deref().unwrap_or("n/a"),
        trace_id = Empty,
        parent.trace_id = Empty,
    );

    toolkit_trace_context::set_parent_from_headers(&span, req.headers());

    let request =
        toolkit_trace_context::with_request_id(request_id, next.run(req).instrument(span));
    INBOUND_TRACEPARENT.scope(inbound, request).await
}

// ---------------------------------------------------------------------------
// Serve
// ---------------------------------------------------------------------------

/// Serve the pre-bound `OoP` listener(s) until `cancel` fires, then drain.
///
/// Binding happens earlier in [`OopHttpServer::start`] so `/healthz` is up
/// before the gear's (possibly slow) `start()` phase; this loop only drives the
/// accept + graceful-shutdown machinery:
/// 1. Serve with graceful shutdown wired to `cancel`.
/// 2. On `cancel`: flip readiness to draining (via the caller-owned
///    `ReadinessState`), wait up to `drain_timeout` for in-flight to reach zero,
///    then let `axum` close the listener.
///
/// The `DirectoryService` deregistration is orchestrated by [`OopHttpServer::join`]
/// so the full drain sequence (`cpt-cf-component-oop-bootstrap`) is honored.
///
/// # Errors
/// Returns an error if the server task fails.
async fn serve_loop(
    listener: tokio::net::TcpListener,
    router: Router,
    drain_guard: DrainGuard,
    sidecar: Option<(tokio::net::TcpListener, Router)>,
    drain_timeout: Duration,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    // Optional sidecar probe listener (bound by the caller).
    let sidecar = match sidecar {
        Some((probe_listener, probe_router)) => {
            let shutdown = {
                let cancel = cancel.clone();
                async move { cancel.cancelled().await }
            };
            Some(tokio::spawn(async move {
                if let Err(e) = axum::serve(probe_listener, probe_router)
                    .with_graceful_shutdown(shutdown)
                    .await
                {
                    tracing::warn!(error = %e, "OoP probe sidecar server error");
                }
            }))
        }
        None => None,
    };

    // Main server: graceful shutdown waits for cancellation, then drains.
    let shutdown = {
        let cancel = cancel.clone();
        let guard = drain_guard.clone();
        async move {
            cancel.cancelled().await;
            // Drain step 1/2: flip readiness to 503 and start rejecting new work.
            guard.begin_drain();
            tracing::info!("OoP HTTP server draining (graceful shutdown)");
            // Drain step 3: wait for in-flight requests to complete.
            drain_in_flight(&guard, drain_timeout).await;
        }
    };

    let result = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(anyhow::Error::from);

    if let Some(handle) = sidecar {
        handle.abort();
        if let Err(e) = handle.await
            && !e.is_cancelled()
        {
            tracing::warn!(error = %e, "OoP probe sidecar task join error");
        }
    }

    result
}

/// Owns an `OoP` gear's HTTP surface across the startup boundary.
///
/// [`start`](Self::start) binds the listener and serves the framework probes
/// **immediately** — `/healthz` → `200`, `/readyz` → `starting`/`503`, and gear
/// routes → `503 starting` — so the kubelet's liveness probe passes while the
/// gear's (possibly slow) `start()` phase runs. Once the gear router is composed,
/// [`attach`](Self::attach) swaps it in atomically (no port rebind) and starts
/// background directory presence + dependency resolution. [`join`](Self::join)
/// waits for the graceful drain on shutdown, then deregisters from
/// `DirectoryService` (drain step 4, `cpt-cf-component-oop-bootstrap`).
///
/// Steps 5–7 of the drain order (reverse-dependency wait, stopping runtime
/// services, process exit) are the caller's / operator's responsibility;
/// the presence task stops when `cancel` fires.
pub(super) struct OopHttpServer {
    readiness: Arc<ReadinessState>,
    late: LateRoutes,
    drain_guard: DrainGuard,
    options: OopServeOptions,
    cancel: CancellationToken,
    registration_task: Option<tokio::task::JoinHandle<()>>,
    /// The `OpenAPI` document `registration_task` registers, and the channel
    /// replacing its registration info.
    presence_openapi: Option<Arc<str>>,
    presence_updates: Option<tokio::sync::watch::Sender<RegisterInstanceInfo>>,
    /// Writer tasks for co-hosted provider gears other than
    /// `options.gear_name`; each registers its record through the presence
    /// manager, which renews and (on [`join`](Self::join)) deregisters it.
    provider_writers: Vec<(String, tokio::task::JoinHandle<()>)>,
    /// Per-process presence manager: owns lease renewal, the
    /// `Draining` drain signal and deregistration for every record the writer
    /// tasks register. Created eagerly in [`start`](Self::start) and published in
    /// the `ClientHub` by the caller, so co-hosted registrars renew through it.
    presence: Arc<PresenceManager>,
    /// Drives the manager's renewal loop. **Independent of the root `cancel`**
    /// so the manager keeps renewing — and reporting `Draining` — for the whole
    /// drain window. [`join`](Self::join) stops it *just before* the final
    /// deregistration (drain step 4) so an in-flight renewal cannot re-register
    /// a record after it is removed; `Drop` aborts it.
    presence_cancel: CancellationToken,
    presence_run: Option<tokio::task::JoinHandle<()>>,
    /// Taken by [`join`](Self::join); aborted by `Drop` otherwise.
    serve: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}

/// Dropped without [`join`](OopHttpServer::join) — the lifecycle future was
/// dropped — the server closes its listeners and presence instead of
/// outliving the runtime.
impl Drop for OopHttpServer {
    fn drop(&mut self) {
        if let Some(task) = self.registration_task.take() {
            task.abort();
        }
        for (_, task) in self.provider_writers.drain(..) {
            task.abort();
        }
        // Stop the manager's renewal loop (its own token, not the root's).
        self.presence_cancel.cancel();
        if let Some(task) = self.presence_run.take() {
            task.abort();
        }
        if let Some(serve) = self.serve.take() {
            serve.abort();
        }
    }
}

impl OopHttpServer {
    /// Bind the listener(s) and start serving probes immediately; gear routes
    /// reply `503 starting` until [`attach`](Self::attach) publishes them.
    ///
    /// # Errors
    /// Returns an error if the main (or sidecar) listener cannot be bound.
    pub(super) async fn start(
        readiness: Arc<ReadinessState>,
        options: OopServeOptions,
        cancel: CancellationToken,
    ) -> anyhow::Result<Self> {
        let late = LateRoutes::default();
        let drain_guard = DrainGuard::new(Arc::clone(&readiness));
        let mut options = options;

        let listener = tokio::net::TcpListener::bind(options.listen_addr).await?;
        let bound = listener.local_addr()?;
        let configured_port = options.listen_addr.port();
        if bound.port() != configured_port {
            if let Some(rewritten) =
                rewrite_advertise_uri_port(&options.advertise_uri, bound.port(), configured_port)
            {
                tracing::info!(
                    old = %options.advertise_uri,
                    new = %rewritten,
                    "advertise_uri rewritten to use actually bound port"
                );
                options.advertise_uri = rewritten;
            } else {
                tracing::info!(
                    advertise_uri = %options.advertise_uri,
                    bound_port = bound.port(),
                    configured_port,
                    "bound port differs from configured; leaving advertise_uri unchanged"
                );
            }
        }
        options.listen_addr = bound;
        tracing::info!(
            addr = %options.listen_addr,
            "OoP HTTP server bound (probes live; gear routes attach after start)"
        );

        let outer = build_outer_router(Arc::clone(&readiness), late.clone());

        let sidecar = if let Some(addr) = options.probe_bind_addr {
            let probe_listener = tokio::net::TcpListener::bind(addr).await?;
            let bound_probe = probe_listener.local_addr()?;
            options.probe_bind_addr = Some(bound_probe);
            tracing::info!(addr = %bound_probe, "OoP probe sidecar bound");
            Some((
                probe_listener,
                build_probe_router(Arc::clone(&readiness), late.clone()),
            ))
        } else {
            None
        };

        let serve = tokio::spawn(serve_loop(
            listener,
            outer,
            drain_guard.clone(),
            sidecar,
            options.drain_timeout,
            cancel.clone(),
        ));

        // The per-process presence manager is created eagerly, not on the first
        // `start_presence`: co-hosted registrars (grpc-hub, event-broker) register
        // during the startup phases — before any root gear's routes attach — and
        // must resolve the same manager so their records are renewed too. The
        // caller publishes it in the `ClientHub` (see
        // `HostRuntime::run_oop_serving`). Its reported state comes from the gear's
        // *full* readiness (startup, deps, healthchecks, drain — ADR-0005), so a
        // gear failing its readiness check advertises `Starting`, not `Serving`.
        let presence_cancel = CancellationToken::new();
        let presence = PresenceManager::new(
            Arc::clone(&options.directory),
            ReportedStateSource::Readiness(Arc::clone(&readiness)),
            options.heartbeat_interval,
        );
        // The renewal loop runs on `presence_cancel` (independent of the root
        // `cancel`) so it keeps renewing and reporting `Draining` through the
        // whole drain window; `join` stops it just before the final deregister,
        // `Drop` aborts it.
        let presence_run = tokio::spawn(Arc::clone(&presence).run(presence_cancel.clone()));
        // Push an immediate `Draining` heartbeat the moment shutdown begins,
        // rather than waiting for the next renewal tick. Also exits on
        // `presence_cancel` so it never outlives the manager.
        {
            let drain_pm = Arc::clone(&presence);
            let drain_readiness = Arc::clone(&readiness);
            let root = cancel.clone();
            let stop = presence_cancel.clone();
            tokio::spawn(async move {
                tokio::select! {
                    () = root.cancelled() => {
                        // Flip readiness to `Draining` *before* waking the
                        // manager so the immediate renewal reports `Draining`,
                        // not a stale `Serving`. The serve loop also flips it on
                        // cancel; doing it here too (idempotent) removes the race
                        // where this wake wins first.
                        drain_readiness.set_draining(true);
                        drain_pm.begin_drain();
                    }
                    () = stop.cancelled() => {}
                }
            });
        }

        Ok(Self {
            readiness,
            late,
            drain_guard,
            options,
            cancel,
            registration_task: None,
            presence_openapi: None,
            presence_updates: None,
            provider_writers: Vec::new(),
            presence,
            presence_cancel,
            presence_run: Some(presence_run),
            serve: Some(serve),
        })
    }

    /// The serve options (used by the caller to compose the `OpenAPI` document).
    pub(super) fn options(&self) -> &OopServeOptions {
        &self.options
    }

    /// Resolve the tenant-plane authenticator from the populated `ClientHub`.
    ///
    /// Called by the `OoP` serving path after the gear lifecycle's `start`
    /// phase and before [`attach`](Self::attach) layers the middleware. When an
    /// in-process authn stack (e.g. the `authn-resolver` gear) is linked into
    /// the binary it registers a [`DynBearerAuthenticator`] bridge during
    /// `init`; picking it up here installs `security_context_middleware` on the
    /// gear routes. A no-op if an authenticator was already supplied directly or
    /// none is registered in the hub.
    pub(super) fn resolve_bearer_authenticator(&mut self, hub: &crate::ClientHub) {
        if self.options.bearer_authenticator.is_some() {
            return;
        }
        if let Ok(auth) = hub.get::<DynBearerAuthenticator>() {
            self.options.bearer_authenticator = Some((*auth).clone());
            tracing::info!(
                gear = %self.options.gear_name,
                "tenant-plane authenticator installed (security_context_middleware enabled)"
            );
        } else {
            tracing::info!(
                gear = %self.options.gear_name,
                "no tenant-plane authenticator registered in ClientHub"
            );
        }
    }

    /// Refuse to serve tenant operations without a bearer authenticator (unless
    /// `auth_disabled`): a startup error names the cause instead of a listener that
    /// refuses every tenant request. Without one, a route the policy does not list is
    /// refused too, which only a warning can flag: the router cannot list its routes.
    /// Platform operations without an internal authenticator only warn (when layered):
    /// the platform gate already refuses each request, and a linked gear's platform route
    /// nobody calls must not stop the process.
    ///
    /// # Errors
    /// When tenant operations are declared and no tenant authenticator is available.
    pub(super) fn check_route_auth(&self, route_auth: &RouteAuthPolicy) -> anyhow::Result<()> {
        if !matches!(self.options.tenant_plane(), TenantPlane::Absent) {
            return Ok(());
        }
        let gear = &self.options.gear_name;
        let tenant = route_auth.tenant_routes();
        if !tenant.is_empty() {
            anyhow::bail!(
                "gear '{gear}' serves tenant operations but no tenant authenticator is \
                 available (link authn-resolver, or set oop_http.auth_disabled for \
                 development): {tenant:?}"
            );
        }
        tracing::warn!(
            gear = %gear,
            reason = "TENANT_AUTH_NOT_CONFIGURED",
            "no tenant authenticator on this listener: a route not declared through \
             OperationBuilder is refused with 401 (link authn-resolver, or set \
             oop_http.auth_disabled for development)"
        );
        Ok(())
    }

    /// Serve system platform-plane and anonymous routes before application startup;
    /// tenant routes and unknown paths return `503` until tenant auth is installed.
    /// Does not mark startup complete or advertise; see [`start_presence`](Self::start_presence).
    pub(super) fn publish_bootstrap_routes(
        &mut self,
        gear_router: Router,
        openapi_json: String,
        policy: OopRoutePolicy,
    ) {
        // The outer gate blocks tenant calls. Keep anonymous contexts anonymous,
        // even with `auth_disabled`; install tenant auth with the final routes.
        let authenticated = layer_route_auth_with(
            gear_router,
            policy.auth.clone(),
            TenantPlane::Absent,
            self.options.internal_authenticator.clone(),
        );
        let gated = authenticated.layer(from_fn_with_state(
            Arc::new(policy.auth),
            bootstrap_plane_gate,
        ));
        let layered = layer_serving(
            gated,
            self.drain_guard.clone(),
            &self.options,
            policy.timeouts,
        );
        self.late.publish(layered, Arc::from(openapi_json));
        tracing::info!(
            gear = %self.options.gear_name,
            "OoP system routes attached (platform plane only)"
        );
    }

    /// Atomically publish the composed gear routes with their listener policy and
    /// `OpenAPI` document on the bound listener, replacing the bootstrap
    /// routes, and mark startup complete.
    ///
    /// Dependency resolution runs in the proxy-wiring phase before serving;
    /// directory presence is started separately by
    /// [`start_presence`](Self::start_presence).
    ///
    /// # Errors
    /// When the route policy fails [`Self::check_route_auth`]; nothing is published.
    pub(super) fn attach(
        &mut self,
        gear_router: Router,
        openapi_json: String,
        policy: OopRoutePolicy,
    ) -> anyhow::Result<()> {
        // A declared operation whose plane has no validator would refuse every request;
        // stop here, before serving, and say why.
        self.check_route_auth(&policy.auth)?;
        let layered = layer_gear_router(
            gear_router,
            policy,
            self.options.tenant_plane(),
            self.drain_guard.clone(),
            &self.options,
        );
        self.late.publish(layered, Arc::from(openapi_json));
        self.readiness.mark_startup_complete();
        tracing::info!(gear = %self.options.gear_name, "OoP gear routes attached (now serving)");
        Ok(())
    }

    /// The per-process presence manager (created in [`start`](Self::start)).
    ///
    /// Published in the `ClientHub` by the caller so co-hosted registrars renew
    /// through the same manager.
    pub(super) fn presence_manager(&self) -> Arc<PresenceManager> {
        Arc::clone(&self.presence)
    }

    /// Register directory presence with the current `OpenAPI` document; the
    /// presence manager then owns the record's lease renewal, the `Draining`
    /// drain signal and deregistration. Later calls register a changed document
    /// without interrupting renewal, so gateway discovery sees the newly served
    /// routes. An unchanged document is a no-op.
    pub(super) fn start_presence(&mut self) {
        let openapi = self.late.openapi();
        if let Some(updates) = &self.presence_updates {
            if self.presence_openapi != openapi {
                updates.send_replace(self.registration_info(openapi.as_ref()));
                self.presence_openapi = openapi;
            }
            return;
        }
        let registration_info = self.registration_info(openapi.as_ref());
        self.presence_openapi = openapi;
        let (updates, presence_rx) = tokio::sync::watch::channel(registration_info);
        self.presence_updates = Some(updates);
        let manager = self.presence_manager();
        let cancel = self.cancel.clone();
        self.registration_task = Some(tokio::spawn(async move {
            super::oop_registration::presence_loop_with_updates(manager, presence_rx, cancel).await;
        }));
    }

    /// The advertised gear's registration, with `openapi`.
    fn registration_info(&self, openapi: Option<&Arc<str>>) -> RegisterInstanceInfo {
        let mut registration_info = RegisterInstanceInfo::new(
            self.options.gear_name.clone(),
            self.options.instance_id.clone(),
        )
        .with_rest_endpoint(ServiceEndpoint::new(self.options.advertise_uri.clone()))
        .with_labels(self.options.labels.clone());
        if let Some(openapi) = openapi {
            // The directory stores only the hash; the edge fetches the document
            // from this worker's `/.well-known/openapi.json` and verifies it
            // against the hash (ADR-0011). Send the hash, not the ~1 MiB body.
            registration_info = registration_info
                .with_openapi_spec_hash(crate::directory::openapi_spec_hash(openapi.as_bytes()));
        }
        if let Some(version) = self.options.version.clone() {
            registration_info = registration_info.with_version(version);
        }
        registration_info
    }

    /// Advertise a co-hosted provider at this listener's URL and instance id,
    /// even while the root starts. The record registers (with backoff) and is
    /// then renewed and deregistered by the shared presence manager.
    pub(super) fn start_provider_presence(&mut self, gear: String) {
        if gear == self.options.gear_name
            || self.provider_writers.iter().any(|(name, _)| *name == gear)
        {
            return;
        }
        let mut info = RegisterInstanceInfo::new(gear.clone(), self.options.instance_id.clone())
            .with_rest_endpoint(ServiceEndpoint::new(self.options.advertise_uri.clone()))
            .with_labels(self.options.labels.clone());
        if let Some(version) = self.options.version.clone() {
            info = info.with_version(version);
        }
        let manager = self.presence_manager();
        let cancel = self.cancel.clone();
        let task = tokio::spawn(async move {
            super::oop_registration::presence_loop(manager, info, cancel).await;
        });
        self.provider_writers.push((gear, task));
    }

    /// Wait for the server to drain on shutdown, then deregister from
    /// `DirectoryService` (drain step 4 — after in-flight requests finish so
    /// consumers don't see stale "ready" state).
    ///
    /// # Errors
    /// Propagates a serve error; deregistration failures are logged only.
    pub(super) async fn join(mut self) -> anyhow::Result<()> {
        // Awaited in place: if this future is dropped, `Drop` still aborts it.
        let Some(serve) = self.serve.as_mut() else {
            anyhow::bail!("OoP serve task already joined");
        };
        let serve_result = match serve.await {
            Ok(r) => r,
            Err(e) => Err(anyhow::anyhow!("OoP serve task join error: {e}")),
        };
        self.serve = None;

        // The writer tasks stop; through the drain window above the presence
        // manager kept renewing and reporting `Draining`. Stop and join the
        // renewal loop *before* deregistering (drain step 4) so a renewal pass
        // in flight cannot re-register a record after `deregister_all` removes
        // it. The drain window has already elapsed (the serve task above drained
        // in-flight requests), so stopping renewal here does not cut it short.
        if let Some(task) = self.registration_task.take() {
            task.abort();
        }
        for (_, task) in std::mem::take(&mut self.provider_writers) {
            task.abort();
        }
        self.presence_cancel.cancel();
        if let Some(task) = self.presence_run.take()
            && let Err(e) = task.await
        {
            tracing::warn!(error = %e, "presence manager renewal task join error");
        }
        self.presence.deregister_all().await;
        tracing::info!(gear = %self.options.gear_name, "deregistered from DirectoryService");

        serve_result
    }
}

/// Rewrite `advertise_uri` to use the actually bound port, but only if the
/// URI currently uses the `configured_port` (i.e. the port the caller asked
/// the server to bind to). This preserves user-provided load-balancer URLs
/// that intentionally advertise a different port from the local socket.
fn rewrite_advertise_uri_port(
    advertise_uri: &str,
    bound_port: u16,
    configured_port: u16,
) -> Option<String> {
    let mut url = Url::parse(advertise_uri).ok()?;
    if url.port_or_known_default() != Some(configured_port) {
        return None;
    }
    url.set_port(Some(bound_port)).ok()?;
    Some(url.to_string())
}

/// Wait up to `timeout` for the in-flight counter to reach zero.
async fn drain_in_flight(guard: &DrainGuard, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let in_flight = guard.in_flight();
        if in_flight == 0 {
            tracing::info!("OoP drain complete: no in-flight requests");
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                in_flight,
                timeout_secs = timeout.as_secs(),
                "OoP drain timed out with in-flight requests remaining"
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "oop_serve_tests.rs"]
mod tests;
