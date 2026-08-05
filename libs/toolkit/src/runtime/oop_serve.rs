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
//!
//! The server is driven by [`OopHttpServer`], invoked from the `HostRuntime`
//! `OoP` path: it binds and serves the probes **before** the gear's `start()`
//! phase (so `/healthz` is up during a slow startup), then swaps in the composed
//! gear routes. Self-registration and dependency resolution live in
//! [`super::oop_registration`].

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header::RETRY_AFTER},
    middleware::{Next, from_fn, from_fn_with_state},
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::FutureExt as _;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;
use url::Url;

use cf_system_sdks::directory::{DirectoryClient, RegisterInstanceInfo, ServiceEndpoint};
use toolkit_canonical_errors::CanonicalError;
use toolkit_http_middleware::{internal_auth_middleware, security_context_middleware};
use toolkit_security::{
    AuthNError, BearerAuthenticator, InternalAuthNError, InternalAuthenticator, PlatformIdentity,
    SecurityContext,
};

use super::readiness::ReadinessState;
use crate::api::canonical_error_middleware;

/// `Retry-After` (seconds) advertised while the gear is draining.
const DRAIN_RETRY_AFTER_SECONDS: u64 = 5;

/// `Retry-After` (seconds) advertised for gear routes before the gear has
/// finished starting (probes are already live; routes come up shortly).
const STARTING_RETRY_AFTER_SECONDS: u64 = 1;

// ---------------------------------------------------------------------------
// Object-safe authenticator adapters
// ---------------------------------------------------------------------------

type BearerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<SecurityContext, AuthNError>> + Send + 'a>>;
type InternalFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PlatformIdentity, InternalAuthNError>> + Send + 'a>>;

/// Object-safe erasure of [`BearerAuthenticator`].
///
/// [`BearerAuthenticator::authenticate`] returns `impl Future`, so the trait is
/// not `dyn`-compatible. This trait boxes the future so a concrete authenticator
/// can be stored behind an `Arc` and injected at the bootstrap layer.
trait ErasedBearer: Send + Sync {
    fn authenticate<'a>(&'a self, token: &'a str) -> BearerFuture<'a>;
}

impl<A: BearerAuthenticator> ErasedBearer for A {
    fn authenticate<'a>(&'a self, token: &'a str) -> BearerFuture<'a> {
        Box::pin(BearerAuthenticator::authenticate(self, token))
    }
}

/// Injectable, object-safe tenant-plane authenticator.
///
/// Wrap a concrete [`BearerAuthenticator`] (e.g. an `AuthNResolverClient`
/// adapter supplied by the app/gear binary) with [`DynBearerAuthenticator::new`]
/// and hand it to [`OopServeOptions::bearer_authenticator`].
#[derive(Clone)]
pub struct DynBearerAuthenticator(Arc<dyn ErasedBearer>);

impl DynBearerAuthenticator {
    /// Wrap a concrete [`BearerAuthenticator`] in the object-safe adapter.
    #[must_use]
    pub fn new<A: BearerAuthenticator + 'static>(authenticator: A) -> Self {
        Self(Arc::new(authenticator))
    }

    /// Wrap an already-`Arc`'d [`BearerAuthenticator`] in the object-safe adapter.
    #[must_use]
    pub fn from_arc<A: BearerAuthenticator + 'static>(authenticator: Arc<A>) -> Self {
        // Adapt Arc<A> to Arc<dyn ErasedBearer> via a thin wrapper.
        struct W<A>(Arc<A>);
        impl<A: BearerAuthenticator> ErasedBearer for W<A> {
            fn authenticate<'a>(&'a self, token: &'a str) -> BearerFuture<'a> {
                Box::pin(BearerAuthenticator::authenticate(&*self.0, token))
            }
        }
        Self(Arc::new(W(authenticator)))
    }
}

impl BearerAuthenticator for DynBearerAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<SecurityContext, AuthNError> {
        self.0.authenticate(token).await
    }
}

/// Object-safe erasure of [`InternalAuthenticator`] (same rationale as
/// [`ErasedBearer`]).
trait ErasedInternal: Send + Sync {
    fn authenticate<'a>(&'a self, token: &'a str) -> InternalFuture<'a>;
}

impl<A: InternalAuthenticator> ErasedInternal for A {
    fn authenticate<'a>(&'a self, token: &'a str) -> InternalFuture<'a> {
        Box::pin(InternalAuthenticator::authenticate(self, token))
    }
}

/// Injectable, object-safe platform-plane authenticator.
///
/// Wrap a concrete [`InternalAuthenticator`] (e.g. the K8s `TokenReview`
/// validator) with [`DynInternalAuthenticator::new`] and hand it to
/// [`OopServeOptions::internal_authenticator`].
#[derive(Clone)]
pub struct DynInternalAuthenticator(Arc<dyn ErasedInternal>);

impl DynInternalAuthenticator {
    /// Wrap a concrete [`InternalAuthenticator`] in the object-safe adapter.
    #[must_use]
    pub fn new<A: InternalAuthenticator + 'static>(authenticator: A) -> Self {
        Self(Arc::new(authenticator))
    }

    /// Wrap an already-`Arc`'d [`InternalAuthenticator`] in the object-safe adapter.
    #[must_use]
    pub fn from_arc<A: InternalAuthenticator + 'static>(authenticator: Arc<A>) -> Self {
        struct W<A>(Arc<A>);
        impl<A: InternalAuthenticator> ErasedInternal for W<A> {
            fn authenticate<'a>(&'a self, token: &'a str) -> InternalFuture<'a> {
                Box::pin(InternalAuthenticator::authenticate(&*self.0, token))
            }
        }
        Self(Arc::new(W(authenticator)))
    }
}

impl InternalAuthenticator for DynInternalAuthenticator {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        self.0.authenticate(token).await
    }
}

// ---------------------------------------------------------------------------
// Serve options
// ---------------------------------------------------------------------------

/// Configuration and collaborators for the `OoP` HTTP runtime, assembled by the
/// bootstrap layer and consumed by `HostRuntime`'s `OoP` serving path.
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
    /// Interval between `DirectoryService` heartbeats. The single presence task
    /// ([`presence_loop`](super::oop_registration)) sends a heartbeat every
    /// interval to keep the instance `Healthy`/routable and avoid
    /// heartbeat-timeout eviction. The presence loop clamps this to a 1s minimum.
    pub heartbeat_interval: Duration,
    /// Per-check timeout for readiness healthchecks (`/readyz`). A gear's
    /// [`Healthcheck`](crate::healthcheck::Healthcheck) that exceeds this is
    /// reported `Unhealthy`. Mirrors the `api-gateway` `healthcheck_timeout_ms`.
    pub healthcheck_timeout: Duration,
    /// Directory client used for self-registration and dependency resolution.
    pub directory: Arc<dyn DirectoryClient>,
    /// Tenant-plane authenticator; when `Some`, `security_context_middleware`
    /// is installed on gear routes.
    pub bearer_authenticator: Option<DynBearerAuthenticator>,
    /// Platform-plane authenticator; when `Some`, `internal_auth_middleware` is
    /// installed on gear routes (runs before the tenant plane).
    pub internal_authenticator: Option<DynInternalAuthenticator>,
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
            .field("bearer_authenticator", &self.bearer_authenticator.is_some())
            .field(
                "internal_authenticator",
                &self.internal_authenticator.is_some(),
            )
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

/// Apply the framework middleware stack to the composed gear router: auth planes
/// (when injected), the drain guard, and the canonical-error layer.
///
/// Probes are NOT layered here — they live on the outer router (see
/// [`build_outer_router`]) so they stay unguarded and answerable during startup
/// and drain.
fn layer_gear_router(
    gear_router: Router,
    drain_guard: DrainGuard,
    options: &OopServeOptions,
) -> Router {
    let mut gear = gear_router;

    // Auth planes (installed only when injected). Add tenant plane first so it
    // is *inner* to the platform plane — `internal_auth_middleware` must run
    // BEFORE `security_context_middleware` (`cpt-cf-adr-two-plane-auth`).
    if let Some(bearer) = options.bearer_authenticator.clone() {
        gear = gear.layer(from_fn_with_state(
            Arc::new(bearer),
            security_context_middleware::<DynBearerAuthenticator>,
        ));
    }
    if let Some(internal) = options.internal_authenticator.clone() {
        gear = gear.layer(from_fn_with_state(
            Arc::new(internal),
            internal_auth_middleware::<DynInternalAuthenticator>,
        ));
    }

    // Drain guard sits just inside the canonical-error layer: it rejects new
    // requests while draining and tracks the in-flight count (catching handler
    // panics so the counter can never leak and stall the drain).
    gear = gear.layer(from_fn_with_state(drain_guard, drain_guard_middleware));

    // Canonical-error middleware is the outermost gear layer so it post-processes
    // every gear/auth response into `problem+json` with `trace_id` / `instance`
    // filled — matching the in-process (`api-gateway`) path.
    gear.layer(from_fn(canonical_error_middleware))
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
    serve: tokio::task::JoinHandle<anyhow::Result<()>>,
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

        Ok(Self {
            readiness,
            late,
            drain_guard,
            options,
            cancel,
            registration_task: None,
            serve,
        })
    }

    /// The serve options (used by the caller to compose the `OpenAPI` document).
    pub(super) fn options(&self) -> &OopServeOptions {
        &self.options
    }

    /// Publish the composed gear routes (they go live atomically) and start
    /// background directory presence + dependency resolution.
    ///
    /// Presence (registration + heartbeat) and dep resolution start here — only
    /// once the gear can actually serve — so the directory never advertises a
    /// not-yet-serving instance.
    pub(super) fn attach(&mut self, gear_router: Router, openapi_json: String) {
        let openapi_arc: Arc<str> = Arc::from(openapi_json);
        let layered = layer_gear_router(gear_router, self.drain_guard.clone(), &self.options);
        self.late.publish(layered, Arc::clone(&openapi_arc));
        self.readiness.mark_startup_complete();
        tracing::info!(gear = %self.options.gear_name, "OoP gear routes attached (now serving)");

        // Single directory-presence task: registration + heartbeat + self-heal.
        // Dependency resolution is handled by the proxy-wiring phase (typed
        // `#[toolkit::consumes]` clients), which runs before serving.
        let registration_info = RegisterInstanceInfo {
            gear: self.options.gear_name.clone(),
            instance_id: self.options.instance_id.clone(),
            grpc_services: vec![],
            version: self.options.version.clone(),
            rest_endpoint: Some(ServiceEndpoint::new(self.options.advertise_uri.clone())),
            openapi_spec: Some(openapi_arc.to_string()),
        };
        self.registration_task = Some(tokio::spawn(super::oop_registration::presence_loop(
            Arc::clone(&self.options.directory),
            registration_info,
            self.options.heartbeat_interval,
            self.cancel.clone(),
        )));
    }

    /// Wait for the server to drain on shutdown, then deregister from
    /// `DirectoryService` (drain step 4 — after in-flight requests finish so
    /// consumers don't see stale "ready" state).
    ///
    /// # Errors
    /// Propagates a serve error; deregistration failures are logged only.
    pub(super) async fn join(mut self) -> anyhow::Result<()> {
        let serve_result = match self.serve.await {
            Ok(r) => r,
            Err(e) => Err(anyhow::anyhow!("OoP serve task join error: {e}")),
        };

        if let Some(task) = self.registration_task.take() {
            task.abort();
        }
        if let Err(e) = self
            .options
            .directory
            .deregister_instance(&self.options.gear_name, &self.options.instance_id)
            .await
        {
            tracing::warn!(
                gear = %self.options.gear_name,
                error = %e,
                "deregistration from DirectoryService failed on shutdown"
            );
        } else {
            tracing::info!(gear = %self.options.gear_name, "deregistered from DirectoryService");
        }

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
