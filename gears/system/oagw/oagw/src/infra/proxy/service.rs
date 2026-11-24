use std::borrow::Cow;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use bytes::Bytes;
use credstore_sdk::CredStoreClientV1;
use futures_util::StreamExt;
use http::{HeaderMap, HeaderValue};
use oagw_sdk::body::{Body, BodyStream};
use oagw_sdk::{field, reason};
use tokio_util::sync::CancellationToken;
use toolkit_http_proxy::sanitize::{self, RequestKind};
use toolkit_http_proxy::{ReplayableBody, UpgradeOutcome, empty_body, full_body, stream_body};
use toolkit_security::SecurityContext;

use crate::config::TokenCacheConfig;
use crate::domain::circuit_breaker::{CircuitBreakers, UpstreamPermit};
use crate::domain::error::{Dispatch, DomainError};
use crate::domain::model::{
    PassthroughMode, PathSuffixMode, ProxyTarget, RateLimitConfig, ResponseHeaderRules,
    ShadowedRouteLimit, Upstream,
};
use crate::domain::plugin::credentials::CredentialMode;
use crate::domain::plugin::{
    AuthContext, AuthPlugin, GuardContext, GuardDecision, PluginError, SigningBody,
    TransformErrorContext, TransformRequestContext, TransformResponseContext,
};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::{self, RateLimitBucket, StreamOutcome, phase};
use crate::domain::proxy_path::{PathRewrite, canonical_path, path_rewrite};
use crate::domain::rate_limit::{
    RateLimitKeyContext, RateLimitOutcome, RateLimitResource, RateLimiter, build_pool_key,
    build_rate_limit_key, pool_config,
};
use crate::domain::repo::Tags;
use crate::domain::route_matching::path_after_route_prefix;
use crate::domain::services::management::forbid_http_endpoints;
use crate::domain::services::{
    ControlPlaneService, DataPlaneService, EndpointSelector, ProxyGrant, SelectedEndpoint,
};
use crate::infra::plugin::{AuthPluginRegistry, GuardPluginRegistry, TransformPluginRegistry};

use super::engine::{
    ProxyEngine, TargetError, into_body_stream, is_connect_failure, map_proxy_error,
};
use super::headers;
use super::streaming::StreamShutdown;
use super::{request_builder, secret_ref, signing, streaming};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Default maximum request body size: 100 MB.
const MAX_BODY_SIZE: usize = 100 * 1024 * 1024;
/// Picks one endpoint of the upstream's (`custom-header-routing`).
const TARGET_HOST_HEADER: &str = "x-oagw-target-host";
/// How often idle rate-limit buckets and circuit breakers are evicted.
const IDLE_SWEEP_INTERVAL: Duration = Duration::from_secs(60);

/// Data Plane service implementation: proxy orchestration and plugin execution.
pub struct DataPlaneServiceImpl {
    cp: Arc<dyn ControlPlaneService>,
    /// Reads `request_ref` secrets under the caller's context (ADR-0022).
    credstore: Arc<dyn CredStoreClientV1>,
    backend_selector: Arc<dyn EndpointSelector>,
    engine: Arc<ProxyEngine>,
    /// Ends open SSE streams and WebSocket relays at gear stop and runtime
    /// shutdown ([`Self::stream_shutdown`]).
    stream_shutdown: StreamShutdown,
    auth_registry: AuthPluginRegistry,
    guard_registry: GuardPluginRegistry,
    transform_registry: TransformPluginRegistry,
    rate_limiter: Arc<RateLimiter>,
    /// Shared with the Control Plane, which drops a deleted upstream's
    /// breakers.
    circuit_breakers: Arc<CircuitBreakers>,
    request_timeout: Duration,
    /// Enforces authorization policy before proxying each request.
    policy_enforcer: PolicyEnforcer,
    /// When true, allow HTTP (non-TLS) upstream connections.
    allow_http_upstream: bool,
    /// Maximum request body size in bytes (applies to both buffered and streaming bodies).
    max_body_size: usize,
    /// Idle timeout for WebSocket connections (no data in either direction).
    websocket_idle_timeout: Duration,
    /// Bound on announcing Close 1001 and half-closing each leg at teardown.
    websocket_close_timeout: Duration,
    /// Idle timeout for SSE streaming connections (no data from upstream).
    streaming_idle_timeout: Duration,
    /// Operational metrics port (OTel-backed in production).
    metrics: Arc<dyn OagwMetricsPort>,
}

impl DataPlaneServiceImpl {
    /// Create the data-plane service from its collaborators.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cp: Arc<dyn ControlPlaneService>,
        credstore: Arc<dyn CredStoreClientV1>,
        policy_enforcer: PolicyEnforcer,
        token_http_config: Option<toolkit_http::HttpClientConfig>,
        token_cache_config: TokenCacheConfig,
        backend_selector: Arc<dyn EndpointSelector>,
        engine: Arc<ProxyEngine>,
        metrics: Arc<dyn OagwMetricsPort>,
        rate_limiter: Arc<RateLimiter>,
        circuit_breakers: Arc<CircuitBreakers>,
    ) -> Self {
        let auth_registry = AuthPluginRegistry::with_builtins(
            Arc::clone(&credstore),
            token_http_config,
            token_cache_config,
        );
        let guard_registry = GuardPluginRegistry::with_builtins();
        let transform_registry = TransformPluginRegistry::with_builtins();

        Self {
            cp,
            credstore,
            backend_selector,
            engine,
            stream_shutdown: StreamShutdown::new(),
            auth_registry,
            guard_registry,
            transform_registry,
            rate_limiter,
            circuit_breakers,
            request_timeout: REQUEST_TIMEOUT,
            policy_enforcer,
            allow_http_upstream: false,
            max_body_size: MAX_BODY_SIZE,
            websocket_idle_timeout: Duration::from_secs(300),
            websocket_close_timeout: Duration::from_secs(5),
            streaming_idle_timeout: Duration::from_secs(300),
            metrics,
        }
    }

    /// Start evicting idle rate-limit buckets and circuit breakers every
    /// [`IDLE_SWEEP_INTERVAL`]. The task ends when `cancel` fires or the
    /// limiter, shared with the Control Plane, is dropped.
    pub fn spawn_idle_sweeper(&self, cancel: CancellationToken) {
        tokio::spawn(sweep_idle_state(
            Arc::downgrade(&self.rate_limiter),
            Arc::downgrade(&self.circuit_breakers),
            Arc::clone(&self.metrics),
            IDLE_SWEEP_INTERVAL,
            cancel,
        ));
    }

    /// Override the request timeout.
    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Override the maximum request body size.
    #[must_use]
    pub fn with_max_body_size(mut self, size: usize) -> Self {
        self.max_body_size = size;
        self
    }

    /// The signal the gear ends open SSE streams and WebSocket relays with.
    pub(crate) fn stream_shutdown(&self) -> StreamShutdown {
        self.stream_shutdown.clone()
    }

    /// End open SSE streams and WebSocket relays also when `parent` (the
    /// runtime's root token) is cancelled, the ones opened before the gear's
    /// first start included. Call it before [`Self::stream_shutdown`]: it
    /// replaces the signal.
    #[must_use]
    pub(crate) fn with_streams_under(mut self, parent: &CancellationToken) -> Self {
        self.stream_shutdown = StreamShutdown::under(parent);
        self
    }

    /// Allow HTTP (non-TLS) upstream connections.
    #[must_use]
    pub fn with_allow_http_upstream(mut self, allow: bool) -> Self {
        self.allow_http_upstream = allow;
        self
    }

    /// Override the WebSocket idle timeout.
    #[must_use]
    pub fn with_websocket_idle_timeout(mut self, timeout: Duration) -> Self {
        self.websocket_idle_timeout = timeout;
        self
    }

    /// Override the timeout for announcing a gateway-originated close.
    #[must_use]
    pub fn with_websocket_close_timeout(mut self, timeout: Duration) -> Self {
        self.websocket_close_timeout = timeout;
        self
    }

    /// Override the SSE streaming idle timeout.
    #[must_use]
    pub fn with_streaming_idle_timeout(mut self, timeout: Duration) -> Self {
        self.streaming_idle_timeout = timeout;
        self
    }

    /// Register an auth plugin under `plugin_id`, replacing a built-in of
    /// that id, for pipeline tests.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub(crate) fn with_auth_plugin(mut self, plugin_id: &str, plugin: Arc<dyn AuthPlugin>) -> Self {
        self.auth_registry.insert(plugin_id, plugin);
        self
    }

    /// Register a transform plugin next to the built-ins, for pipeline
    /// tests.
    #[cfg(any(test, feature = "test-utils"))]
    #[must_use]
    pub(crate) fn with_transform_plugin(
        mut self,
        plugin_id: &str,
        plugin: Arc<dyn crate::domain::plugin::TransformPlugin>,
    ) -> Self {
        let registry = std::mem::replace(
            &mut self.transform_registry,
            TransformPluginRegistry::with_builtins(),
        );
        self.transform_registry = registry.with_plugin(plugin_id, plugin);
        self
    }

    /// Apply response transforms, CORS, header rules, and rate-limit headers.
    ///
    /// Shared by both the normal HTTP path (`finalize_response`) and the 101
    /// upgrade path (`handle_websocket_upgrade`).
    async fn apply_response_pipeline(
        &self,
        pipeline: &ResponsePipelineCtx<'_>,
        status: http::StatusCode,
        resp_headers: &mut HeaderMap,
    ) {
        execute_transform_responses(
            &self.transform_registry,
            &pipeline.transform_bindings,
            status,
            resp_headers,
            pipeline.ctx,
        )
        .await;

        // Inject CORS headers for actual (non-preflight) cross-origin requests.
        if let Some(cors_config) = pipeline.cors_config
            && cors_config.enabled
            && let Some(ref origin) = pipeline.origin
        {
            let cors_headers = crate::domain::cors::apply_cors_headers(cors_config, origin);
            for (name, value) in cors_headers {
                if let Ok(v) = HeaderValue::from_str(&value)
                    && let Ok(n) = http::header::HeaderName::from_bytes(name.as_bytes())
                {
                    if n == http::header::VARY {
                        resp_headers.append(n, v);
                    } else {
                        resp_headers.insert(n, v);
                    }
                }
            }
        }

        // Apply response header rules (set/add/remove) from upstream config.
        if let Some(rules) = pipeline.response_header_rules {
            headers::apply_response_header_rules(resp_headers, rules);
        }

        // Inject rate-limit response headers if configured.
        if let Some((ref outcome, true)) = pipeline.rate_limit_outcome {
            resp_headers.insert("x-ratelimit-limit", HeaderValue::from(outcome.limit));
            resp_headers.insert(
                "x-ratelimit-remaining",
                HeaderValue::from(outcome.remaining),
            );
            resp_headers.insert("x-ratelimit-reset", HeaderValue::from(outcome.reset_epoch));
        }
    }

    /// Execute the post-response plugin pipeline (guard + transform) and build
    /// the final proxy response.
    async fn finalize_response(
        &self,
        pipeline: &ResponsePipelineCtx<'_>,
        status: http::StatusCode,
        resp_headers: HeaderMap,
        resp_body_stream: BodyStream,
        instance_uri: String,
    ) -> Result<http::Response<Body>, DomainError> {
        // Decided on the upstream's headers, as in `forward`, so header rules
        // cannot take an SSE body out of its idle timeout.
        let is_sse = oagw_sdk::sse::is_server_events_response(&resp_headers);
        execute_guard_responses(
            &self.guard_registry,
            &pipeline.guard_bindings,
            status,
            &resp_headers,
            pipeline.method,
            pipeline.path_suffix,
            &instance_uri,
            pipeline.ctx,
        )
        .await?;

        let mut resp_headers = resp_headers;
        self.apply_response_pipeline(pipeline, status, &mut resp_headers)
            .await;

        let labels = streaming::StreamLabels {
            alias: pipeline.host.to_owned(),
            path: pipeline.path_suffix.to_owned(),
        };
        // Inside the SSE lifecycle, so the idle and shutdown cuts it adds are
        // not logged as upstream failures.
        let resp_body_stream = streaming::log_upstream_body_error(resp_body_stream, labels.clone());
        // SSE bodies get the streaming idle timeout and end on shutdown. Other
        // bodies keep the engine's read timeout and finish within the HTTP
        // drain.
        let resp_body_stream = if is_sse {
            streaming::streaming_body_with_lifecycle(
                resp_body_stream,
                self.streaming_idle_timeout,
                self.stream_shutdown.subscribe(),
                labels,
            )
        } else {
            resp_body_stream
        };

        build_proxy_response(status, resp_headers, resp_body_stream, instance_uri)
    }

    /// Two-tier endpoint selection (D1):
    /// 1. `X-OAGW-Target-Host` header → validate against endpoint list
    /// 2. Round-robin via `BackendSelector` for multi-endpoint, direct for single
    async fn select_endpoint(
        &self,
        upstream: &Upstream,
        req_headers: &http::HeaderMap,
        instance_uri: &str,
    ) -> Result<SelectedEndpoint, DomainError> {
        let endpoints = &upstream.server.endpoints;

        if endpoints.is_empty() {
            return Err(DomainError::DownstreamError {
                detail: "upstream has no endpoints".into(),
                instance: instance_uri.to_string(),
                dispatch: Dispatch::NotSent,
            });
        }

        // Tier 1: Explicit selection via X-OAGW-Target-Host header.
        if let Some(target_host) = req_headers
            .get(TARGET_HOST_HEADER)
            .and_then(|v| v.to_str().ok())
        {
            // Validate format: allowlist of safe hostname/IP characters.
            // Rejects null bytes, @, \, Unicode homoglyphs, and port/path syntax.
            if target_host.is_empty()
                || !target_host
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
            {
                return Err(DomainError::InvalidTargetHost {
                    instance: instance_uri.to_string(),
                });
            }

            // Find matching endpoint by host (no LB — no resolved addr).
            let endpoint = endpoints
                .iter()
                .find(|ep| ep.host.eq_ignore_ascii_case(target_host))
                .cloned()
                .ok_or_else(|| {
                    let valid_hosts: Vec<&str> =
                        endpoints.iter().map(|ep| ep.host.as_str()).collect();
                    tracing::warn!(
                        target_host,
                        ?valid_hosts,
                        "X-OAGW-Target-Host does not match any configured endpoint"
                    );
                    DomainError::UnknownTargetHost {
                        detail: format!(
                            "X-OAGW-Target-Host '{}' does not match any configured endpoint",
                            target_host
                        ),
                        instance: instance_uri.to_string(),
                    }
                })?;
            return Ok(SelectedEndpoint {
                endpoint,
                resolved_addr: None,
            });
        }

        // Tier 2: Automatic selection.
        if endpoints.len() == 1 {
            // Single-endpoint: bypass LB. `resolved_addr` is None, so the
            // engine adapter resolves DNS (SSRF-filtered) on the request path.
            // Acceptable trade-off: single-endpoint upstreams don't benefit
            // from health-checked LB selection anyway.
            return Ok(SelectedEndpoint {
                endpoint: endpoints[0].clone(),
                resolved_addr: None,
            });
        }

        // Multi-endpoint: round-robin via BackendSelector.
        self.backend_selector
            .select(upstream.id, endpoints)
            .await
            .map_err(|e| DomainError::DownstreamError {
                detail: e.to_string(),
                instance: instance_uri.to_string(),
                dispatch: Dispatch::NotSent,
            })
    }
}

/// Parsed and validated proxy request, ready for resolution and forwarding.
struct ParsedRequest {
    instance_uri: String,
    alias: String,
    path_suffix: String,
    /// What normalizing the client's path suffix removed.
    path_rewrite: PathRewrite,
    query_params: Vec<(String, String)>,
    method: http::Method,
    req_headers: HeaderMap,
    is_upgrade: bool,
    body_bytes: Bytes,
    body_stream: Option<BodyStream>,
    /// Client address from the API gateway's `ClientIp` extension; `None`
    /// for requests that did not come through it (in-process SDK calls).
    client_ip: Option<std::net::IpAddr>,
}

/// Decompose and validate an inbound proxy request.
///
/// Extracts the alias, normalizes the path suffix, parses query parameters,
/// validates headers (Content-Type, Transfer-Encoding), and conditionally
/// buffers or preserves the body stream.
fn parse_proxy_request(
    req: http::Request<Body>,
    max_body_size: usize,
) -> Result<ParsedRequest, DomainError> {
    let instance_uri = req.uri().to_string();

    // Extract alias from the raw path first, then normalize only the
    // suffix. This prevents path traversal (e.g. `/../../admin/...`)
    // from influencing alias extraction.
    let (alias, path_suffix, path_rewrite) = {
        let path = req.uri().path();
        let trimmed = path.strip_prefix('/').unwrap_or(path);
        let (alias, raw_suffix) = match trimmed.find('/') {
            Some(pos) => (&trimmed[..pos], &trimmed[pos..]),
            None => (trimmed, ""),
        };
        (
            alias.to_string(),
            canonical_path(raw_suffix, &instance_uri)?,
            path_rewrite(raw_suffix),
        )
    };

    // Parse query parameters with proper URL decoding.
    let query_params: Vec<(String, String)> = req
        .uri()
        .query()
        .map(|q| {
            form_urlencoded::parse(q.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect()
        })
        .unwrap_or_default();

    // Decompose request into parts. Keep body as-is for conditional handling.
    let (parts, body) = req.into_parts();
    let method = parts.method;
    let req_headers = parts.headers;
    let client_ip = parts
        .extensions
        .get::<toolkit::http::ClientIp>()
        .map(|ip| ip.0);

    let is_upgrade = headers::is_websocket_upgrade(&req_headers);

    // Validate Content-Type format if present.
    headers::validate_content_type(&req_headers)?;

    // HTTP smuggling prevention (RFC 7230 §3.3.3): validate CL, TE, and
    // reject CL/TE co-existence.
    headers::validate_smuggling_headers(&req_headers)?;

    // Conditional body conversion — keep streams for streaming request bodies.
    let (body_bytes, body_stream): (Bytes, Option<BodyStream>) = match body {
        Body::Empty => (Bytes::new(), None),
        Body::Bytes(b) => {
            if b.len() > max_body_size {
                return Err(DomainError::PayloadTooLarge {
                    detail: format!(
                        "request body of {} bytes exceeds maximum of {max_body_size} bytes",
                        b.len()
                    ),
                    instance: instance_uri,
                });
            }
            (b, None)
        }
        Body::Stream(s) => (Bytes::new(), Some(s)),
    };

    Ok(ParsedRequest {
        instance_uri,
        alias,
        path_suffix,
        path_rewrite,
        query_params,
        method,
        req_headers,
        is_upgrade,
        body_bytes,
        body_stream,
        client_ip,
    })
}

/// Where a `proxy:invoke` check stands, for [`AuthorizeMetricsGuard`].
enum AuthorizeOutcome {
    /// Not answered yet: a drop now is a cancellation.
    Pending,
    /// Granted: the proxied request is counted by [`RequestMetricsGuard`].
    Granted,
    /// Refused or failed, under this error label and response status.
    Failed { variant: &'static str, status: u16 },
}

/// Counts a `proxy:invoke` check that does not lead to a proxied request:
/// a denial or failure, a check whose future was dropped, or a granted
/// request the REST handler rejected before proxying it. A granted check
/// records nothing here.
struct AuthorizeMetricsGuard {
    metrics: Arc<dyn OagwMetricsPort>,
    start: Instant,
    method: &'static str,
    outcome: AuthorizeOutcome,
}

impl AuthorizeMetricsGuard {
    fn new(metrics: Arc<dyn OagwMetricsPort>, method: &'static str) -> Self {
        Self {
            metrics,
            start: Instant::now(),
            method,
            outcome: AuthorizeOutcome::Pending,
        }
    }

    /// Count a request granted `authorized_in` ago and rejected before it
    /// was proxied. It is timed as a proxied request is: the check, not the
    /// client's upload.
    fn record_rejected(
        metrics: Arc<dyn OagwMetricsPort>,
        method: &'static str,
        authorized_in: Duration,
        err: &DomainError,
    ) {
        let now = Instant::now();
        let mut guard = Self {
            metrics,
            start: now.checked_sub(authorized_in).unwrap_or(now),
            method,
            outcome: AuthorizeOutcome::Pending,
        };
        guard.failed(err);
    }

    fn granted(&mut self) {
        self.outcome = AuthorizeOutcome::Granted;
    }

    fn failed(&mut self, err: &DomainError) {
        self.outcome = AuthorizeOutcome::Failed {
            variant: domain_error_type_name(err),
            status: domain_error_status(err),
        };
    }
}

impl Drop for AuthorizeMetricsGuard {
    fn drop(&mut self) {
        let (variant, status) = match self.outcome {
            AuthorizeOutcome::Granted => return,
            AuthorizeOutcome::Pending => (metric_labels::CANCELLED, None),
            AuthorizeOutcome::Failed { variant, status } => (variant, Some(status)),
        };
        let unknown = metric_labels::UNKNOWN;
        self.metrics.record_request_duration_seconds(
            unknown,
            unknown,
            phase::TOTAL,
            self.start.elapsed().as_secs_f64(),
        );
        if let Some(status) = status {
            self.metrics
                .record_request(unknown, unknown, self.method, status, None);
        }
        self.metrics.record_error(unknown, unknown, variant, None);
    }
}

/// The part of `path` after the matched route's prefix. The route was chosen
/// by the same rule, so a path outside the prefix is an internal error, not
/// an empty suffix.
fn route_suffix<'a>(path: &'a str, route_prefix: &str) -> Result<&'a str, DomainError> {
    path_after_route_prefix(path, route_prefix).ok_or_else(|| DomainError::Internal {
        message: format!("matched route prefix {route_prefix:?} does not cover the request path"),
    })
}

/// How a proxied request is authorized.
enum Authorization {
    /// Check `proxy:invoke` for this caller first (the SDK path).
    Check(SecurityContext),
    /// Already granted by `authorize_proxy` (the REST path).
    Granted(ProxyGrant),
}

/// Terminal outcome captured before [`RequestMetricsGuard`] drops.
///
/// `Pending` is the unwritten state — the guard treats it as a cancellation
/// (future dropped before a result was produced) and records an error.
enum RequestOutcome {
    Pending,
    Success(u16),
    /// A gateway error: its variant and the status the client gets.
    Error {
        variant: &'static str,
        status: u16,
    },
}

/// One request counted in `requests_in_flight` under `host`; dropping it
/// takes the request out.
struct InFlight {
    metrics: Arc<dyn OagwMetricsPort>,
    host: Cow<'static, str>,
}

impl InFlight {
    fn start(metrics: Arc<dyn OagwMetricsPort>, host: Cow<'static, str>) -> Self {
        metrics.increment_in_flight(&host);
        Self { metrics, host }
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.metrics.decrement_in_flight(&self.host);
    }
}

/// RAII guard that owns the per-request metric bookkeeping for
/// [`DataPlaneServiceImpl::proxy_request`].
///
/// On construction it captures the start time and counts the request in
/// flight under the unknown host; [`Self::mark_resolved`] sets the labels
/// and moves it to the upstream's alias. Before the request returns, the
/// body calls [`Self::set_success`] or [`Self::set_error`] to record the
/// terminal outcome, and [`Self::observe_body`] hands the in-flight count
/// to the response body. `Drop` records the duration to the response head
/// and the request or error counter — even if the future is cancelled or
/// panics — and ends the in-flight count unless the body took it.
struct RequestMetricsGuard {
    metrics: Arc<dyn OagwMetricsPort>,
    start: Instant,
    method: &'static str,
    host: String,
    path: String,
    /// `credential_mode` label: `None` (`unknown`) until the upstream
    /// resolves.
    credential_mode: Option<CredentialMode>,
    in_flight: Option<InFlight>,
    outcome: RequestOutcome,
}

impl RequestMetricsGuard {
    fn new(metrics: Arc<dyn OagwMetricsPort>, method: &'static str) -> Self {
        Self {
            in_flight: Some(InFlight::start(
                Arc::clone(&metrics),
                Cow::Borrowed(metric_labels::UNKNOWN),
            )),
            metrics,
            start: Instant::now(),
            method,
            host: metric_labels::UNKNOWN.to_owned(),
            path: metric_labels::UNKNOWN.to_owned(),
            credential_mode: None,
            outcome: RequestOutcome::Pending,
        }
    }

    fn mark_resolved(&mut self, host: String, path: String, credential_mode: CredentialMode) {
        // Counted under the alias before the unknown entry goes, so the
        // total never dips.
        self.in_flight = Some(InFlight::start(
            Arc::clone(&self.metrics),
            Cow::Owned(host.clone()),
        ));
        self.host = host;
        self.path = path;
        self.credential_mode = Some(credential_mode);
    }

    fn set_success(&mut self, status: u16) {
        self.outcome = RequestOutcome::Success(status);
    }

    fn set_error(&mut self, err: &DomainError) {
        self.outcome = RequestOutcome::Error {
            variant: domain_error_type_name(err),
            status: domain_error_status(err),
        };
    }

    /// Keep the request in flight until the response body ends, and record
    /// `stream_duration_seconds` then. A response without a body (101,
    /// HEAD, 204, 304, `Content-Length: 0`) ends at its head and records no
    /// stream duration.
    fn observe_body(
        &mut self,
        resp: http::Response<Body>,
        method: &http::Method,
    ) -> http::Response<Body> {
        let status = resp.status();
        // The header the client gets: header rules and transforms run
        // before this and may have changed or removed it.
        let declared_len = declared_content_length(resp.headers());
        let has_body = *method != http::Method::HEAD
            && !status.is_informational()
            && status != http::StatusCode::NO_CONTENT
            && status != http::StatusCode::NOT_MODIFIED
            && declared_len != Some(0);
        let (parts, body) = resp.into_parts();
        let body = match body {
            Body::Stream(stream) if has_body => {
                let end = StreamEnd {
                    metrics: Arc::clone(&self.metrics),
                    host: self.host.clone(),
                    path: self.path.clone(),
                    start: self.start,
                };
                let in_flight = self.in_flight.take();
                Body::Stream(streaming::on_body_end(
                    stream,
                    declared_len,
                    move |outcome| {
                        end.record(outcome);
                        drop(in_flight);
                    },
                ))
            }
            // hyper does not read the body of these (it would end as
            // cancelled): the request ends at its head.
            body @ Body::Stream(_) => body,
            // A 101 carries no body; the session has its own metrics.
            body @ Body::Empty => body,
            // Built whole by the gateway, not relayed: nothing to wait for.
            body @ Body::Bytes(_) => body,
        };
        http::Response::from_parts(parts, body)
    }
}

/// The single `Content-Length` of a response, if it has one that parses.
fn declared_content_length(headers: &HeaderMap) -> Option<u64> {
    let mut values = headers.get_all(http::header::CONTENT_LENGTH).iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    value.to_str().ok()?.trim().parse().ok()
}

/// Labels and start time for `stream_duration_seconds`.
struct StreamEnd {
    metrics: Arc<dyn OagwMetricsPort>,
    host: String,
    path: String,
    start: Instant,
}

impl StreamEnd {
    fn record(&self, outcome: StreamOutcome) {
        self.metrics.record_stream_duration_seconds(
            &self.host,
            &self.path,
            outcome,
            self.start.elapsed().as_secs_f64(),
        );
    }
}

impl Drop for RequestMetricsGuard {
    fn drop(&mut self) {
        // A request without a body leaves the in-flight count first.
        drop(self.in_flight.take());
        self.metrics.record_request_duration_seconds(
            &self.host,
            &self.path,
            phase::TOTAL,
            self.start.elapsed().as_secs_f64(),
        );
        match self.outcome {
            RequestOutcome::Success(status) => {
                self.metrics.record_request(
                    &self.host,
                    &self.path,
                    self.method,
                    status,
                    self.credential_mode,
                );
            }
            RequestOutcome::Error { variant, status } => {
                self.metrics.record_request(
                    &self.host,
                    &self.path,
                    self.method,
                    status,
                    self.credential_mode,
                );
                self.metrics
                    .record_error(&self.host, &self.path, variant, self.credential_mode);
            }
            RequestOutcome::Pending => {
                self.metrics.record_error(
                    &self.host,
                    &self.path,
                    metric_labels::CANCELLED,
                    self.credential_mode,
                );
            }
        }
    }
}

#[async_trait]
impl DataPlaneService for DataPlaneServiceImpl {
    async fn proxy_request(
        &self,
        ctx: SecurityContext,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, DomainError> {
        self.proxy(Authorization::Check(ctx), req).await
    }

    async fn authorize_proxy(
        &self,
        ctx: SecurityContext,
        method: &http::Method,
    ) -> Result<ProxyGrant, DomainError> {
        // cancel-safe: the guard records a denial or a cancellation (the
        // future dropped mid-check, e.g. by the gateway's request timeout)
        // when it drops. A granted request is counted by
        // `proxy_request_granted`, by `reject_granted` when the caller
        // rejects it first, or by the grant itself when it is dropped.
        let method = crate::infra::metrics::normalize_method(method);
        let mut guard = AuthorizeMetricsGuard::new(Arc::clone(&self.metrics), method);
        let result = ProxyGrant::authorize(&self.policy_enforcer, ctx).await;
        match &result {
            Ok(_) => guard.granted(),
            Err(e) => guard.failed(e),
        }
        result.map(|grant| grant.count_if_dropped(Arc::clone(&self.metrics), method))
    }

    fn reject_granted(&self, mut grant: ProxyGrant, err: &DomainError) {
        if let Some((metrics, method)) = grant.take_count() {
            AuthorizeMetricsGuard::record_rejected(metrics, method, grant.authorized_in(), err);
        }
    }

    async fn proxy_request_granted(
        &self,
        grant: ProxyGrant,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, DomainError> {
        self.proxy(Authorization::Granted(grant), req).await
    }
}

impl DataPlaneServiceImpl {
    /// The proxy pipeline. Without a grant, `proxy:invoke` is checked first,
    /// before the request is validated.
    async fn proxy(
        &self,
        mut auth: Authorization,
        req: http::Request<Body>,
    ) -> Result<http::Response<Body>, DomainError> {
        let request_method = req.method().clone();
        let method = crate::infra::metrics::normalize_method(&request_method);
        let mut guard = RequestMetricsGuard::new(Arc::clone(&self.metrics), method);
        if let Authorization::Granted(grant) = &mut auth {
            // The guard counts the request from here on.
            grant.take_count();
            // The REST path authorized before reading the body: count the
            // check, as the SDK path does, but not the client's upload.
            guard.start = guard
                .start
                .checked_sub(grant.authorized_in())
                .unwrap_or(guard.start);
        }
        let mut host_label: String = metric_labels::UNKNOWN.to_owned();
        let mut path_label: String = metric_labels::UNKNOWN.to_owned();

        let result: Result<http::Response<Body>, DomainError> = async {
            let grant = match auth {
                Authorization::Granted(grant) => grant,
                Authorization::Check(ctx) => ProxyGrant::authorize(&self.policy_enforcer, ctx).await?,
            };
            let ctx = grant.ctx();

            let ParsedRequest {
                instance_uri,
                alias,
                path_suffix,
                path_rewrite,
                mut query_params,
                method,
                req_headers,
                is_upgrade,
                body_bytes,
                body_stream,
                client_ip,
            } = parse_proxy_request(req, self.max_body_size)?;

            let max_body = self.max_body_size;

            // 1. Resolve upstream + route in one pass (single hierarchy walk),
            //    timed: phase="resolve".
            //
            // Recorded on the error path too, under the unknown labels: a
            // request that did not resolve has no upstream or route to name.
            let resolve_start = Instant::now();
            let resolved = self
                .cp
                .resolve_proxy_target(&grant, &alias, method.as_ref(), &path_suffix, Tags::Skip)
                .await;
            let resolve_seconds = resolve_start.elapsed().as_secs_f64();
            let ProxyTarget {
                upstream,
                route,
                shadowed_route_limits,
                tenant_chain,
                auth_owner_tenant_id,
            } = match resolved {
                Ok(target) => target,
                Err(e) => {
                    self.metrics.record_request_duration_seconds(
                        metric_labels::UNKNOWN,
                        metric_labels::UNKNOWN,
                        phase::RESOLVE,
                        resolve_seconds,
                    );
                    return Err(e);
                }
            };

            // Capture metric labels now that upstream + route are known, and
            // move the in-flight count to the upstream's alias.
            host_label = upstream.alias.clone();
            path_label = route
                .match_rules
                .http
                .as_ref()
                .map_or("/", |h| h.path.as_str())
                .to_owned();
            let credential_mode = CredentialMode::of(upstream.auth.as_ref());
            guard.mark_resolved(host_label.clone(), path_label.clone(), credential_mode);
            self.metrics.record_request_duration_seconds(
                &host_label,
                &path_label,
                phase::RESOLVE,
                resolve_seconds,
            );

        // 1b. Per-request secret reference (ADR-0022): the header's syntax,
        //     then the upstream's mode. Both before any capacity is taken or
        //     CredStore read.
        let requested_ref = secret_ref::requested_ref(&req_headers, &instance_uri)?;
        let request_ref = secret_ref::select(
            credential_mode,
            upstream.auth.as_ref(),
            requested_ref,
            &instance_uri,
        )?;

        // 2. CORS origin enforcement for actual cross-origin requests.
        // Preflight is handled permissively at the handler level (no upstream resolution).
        let effective_cors = upstream.cors.clone();
        let request_origin = req_headers
            .get(http::header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        enforce_cors_origin(
            effective_cors.as_ref(),
            request_origin.as_deref(),
            method.as_ref(),
            &instance_uri,
        )?;

        // 3. Validate query parameters against route's allowlist.
        if let Some(ref http_match) = route.match_rules.http
            && !query_params.is_empty()
        {
            for (key, _) in &query_params {
                if !http_match.query_allowlist.contains(key) {
                    return Err(DomainError::Validation {
                        field: "query",
                        reason: field::QUERY_NOT_ALLOWED,
                        detail: format!(
                            "query parameter '{}' is not in the route's query_allowlist",
                            key
                        ),
                        instance: instance_uri,
                    });
                }
            }
        }

        // 3b. Enforce path_suffix_mode.
        if let Some(ref http_match) = route.match_rules.http
            && http_match.path_suffix_mode == PathSuffixMode::Disabled
        {
            let route_path = &http_match.path;
            let extra = route_suffix(&path_suffix, route_path)?;
            if !extra.is_empty() {
                return Err(DomainError::Validation {
                    field: "path",
                    reason: field::PATH_SUFFIX_NOT_ALLOWED,
                    detail: format!(
                        "path suffix not allowed: route path_suffix_mode is disabled but request has extra path '{}'",
                        extra
                    ),
                    instance: instance_uri,
                });
            }
        }

        // 3c. Enforce HTTPS-only before the auth plugin runs, so no credential
        //     is prepared for a plaintext upstream. Writes refuse such
        //     endpoints too; this covers rows stored before the flag was off.
        //     All endpoints of a pool share one scheme.
        forbid_http_endpoints(
            &upstream.server.endpoints,
            self.allow_http_upstream,
            &instance_uri,
        )?;

        // 3d. Circuit breaker (`inst-rl-3`): an open circuit fails fast,
        //     before the auth plugin, the rate limits and the CredStore read.
        //     No probe slot is kept: the permit is taken at step 8a. At the
        //     open → half-open edge, requests that pass here before the first
        //     probe takes its slot at 8a do the steps in between and get 503
        //     at 8a; once a probe holds the slot, this check rejects.
        self.circuit_breakers.check(&upstream, &instance_uri)?;

        // 4. Prepare outbound headers (passthrough + strip).
        let mode = upstream
            .headers
            .as_ref()
            .and_then(|h| h.request.as_ref())
            .map_or(PassthroughMode::None, |r| r.passthrough);
        let allowlist: Vec<String> = upstream
            .headers
            .as_ref()
            .and_then(|h| h.request.as_ref())
            .map_or_else(Vec::new, |r| r.passthrough_allowlist.clone());
        let mut outbound_headers = headers::apply_passthrough(&req_headers, &mode, &allowlist);
        headers::strip_inbound_nominated(&mut outbound_headers, &req_headers, is_upgrade);
        if is_upgrade {
            headers::strip_hop_by_hop_for_upgrade(&mut outbound_headers);
        } else {
            headers::strip_hop_by_hop(&mut outbound_headers);
        }
        headers::strip_internal_headers(&mut outbound_headers);

        // For WebSocket, ensure Upgrade and Sec-WebSocket-* headers are forwarded
        // even when passthrough mode is None/Allowlist.
        if is_upgrade {
            for name in &[
                "upgrade",
                "sec-websocket-key",
                "sec-websocket-version",
                "sec-websocket-protocol",
                "sec-websocket-extensions",
            ] {
                if let Ok(n) = http::header::HeaderName::from_bytes(name.as_bytes())
                    && !outbound_headers.contains_key(&n)
                {
                    for v in req_headers.get_all(&n) {
                        outbound_headers.append(n.clone(), v.clone());
                    }
                }
            }
        }

        // 5. Execute auth plugin (timed: phase="auth").
        //
        // Recorded even on the error path so failures and successes are both
        // visible in the latency histogram. We intentionally only observe
        // when an auth plugin is configured — observing on every request
        // would skew the bucket distribution with zero-cost "no auth" rows.
        //
        // In `request_ref` mode the proxy reads the secret itself (ADR-0022):
        // rate admission here, so an exhausted limit does not load the
        // credential store; the read under the caller's context and the
        // owner-chain and type guards run after the guard plugins (5b'), so a
        // guard rejection does not load it either. `config` mode keeps its
        // order.
        let mut early_rate_limit = None;
        let mut signer = None;
        if request_ref.is_some() {
            early_rate_limit = Some(check_rate_limits(
                &self.rate_limiter,
                &upstream,
                &route,
                &shadowed_route_limits,
                client_ip,
                ctx,
                &instance_uri,
                self.metrics.as_ref(),
                &host_label,
                &path_label,
            )?);
        } else if let Some(ref auth) = upstream.auth {
            let auth_start = std::time::Instant::now();
            let auth_result = execute_auth_plugin(
                &self.auth_registry,
                &upstream,
                auth,
                &mut outbound_headers,
                ctx,
                &instance_uri,
            )
            .await;
            self.metrics.record_request_duration_seconds(
                &host_label,
                &path_label,
                phase::AUTH,
                auth_start.elapsed().as_secs_f64(),
            );
            let plugin = auth_result?;
            signer = plugin.signs().then_some((plugin, auth));
        }

        // 5b. Execute guard plugins (upstream then route).
        //
        // Guards are blocking gates: a rejection short-circuits the pipeline
        // immediately. Guards enforce hard policies (allowlists, rate limits,
        // schema validation). Compare with transforms (step 5d) which use
        // log-and-continue semantics.
        let guard_bindings =
            collect_plugin_bindings(&upstream, GuardPluginRegistry::is_guard_plugin);

        // Guards check what the client sent (ADR-0017), not the outbound
        // map: that starts filtered and, in `config` mode, already carries
        // the credential.
        execute_guard_requests(
            &self.guard_registry,
            &guard_bindings,
            &req_headers,
            method.as_str(),
            &path_suffix,
            ctx,
            &instance_uri,
        )
        .await?;

        // 5b'. `request_ref` CredStore read and owner-chain and type guards
        //      (ADR-0022), after the guard plugins, which need no secret.
        //      The admission (step 5), the key and the deadline exist together
        //      or not at all: one `RequestRefState`.
        let request_ref_state = if let Some((plan, rate_limit)) =
            request_ref.as_ref().zip(early_rate_limit)
        {
            let auth_start = std::time::Instant::now();
            // The request timeout starts at the CredStore read, which shares
            // it with selection and the exchange.
            let deadline = tokio::time::Instant::now() + self.request_timeout;
            let scope = secret_ref::ResolveScope {
                tenant_chain: &tenant_chain,
                auth_owner_tenant_id,
                upstream_id: upstream.id,
                route_id: route.id,
                request_id: req_headers
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok()),
            };
            let resolved = secret_ref::resolve(
                self.credstore.as_ref(),
                ctx,
                plan,
                &scope,
                deadline,
                &instance_uri,
            )
            .await;
            self.metrics.record_request_duration_seconds(
                &host_label,
                &path_label,
                phase::AUTH,
                auth_start.elapsed().as_secs_f64(),
            );
            let secret = resolved?;
            // Guard 6: header rules and transforms never see the key. Strip
            // the auth header now; the key goes in after the final header
            // setup (step 6b).
            secret.strip(&mut outbound_headers);
            Some(RequestRefState {
                rate_limit,
                secret,
                deadline,
            })
        } else {
            None
        };

        // 5c. Collect transform plugin bindings (upstream then route).
        let transform_bindings =
            collect_plugin_bindings(&upstream, TransformPluginRegistry::is_transform_plugin);

        // 5d. Apply header rules.
        if let Some(ref hc) = upstream.headers
            && let Some(ref rules) = hc.request
        {
            headers::apply_request_header_rules(&mut outbound_headers, rules);
        }

        // 5e. Execute transform plugins (on_request phase).
        //
        // Placed after header rules so transforms have the final word on
        // outbound headers. Errors are logged and skipped — transforms use
        // log-and-continue semantics. Compare with guards (step 5b) which
        // fail-hard.
        execute_transform_requests(
            &self.transform_registry,
            &transform_bindings,
            &mut outbound_headers,
            &mut query_params,
            method.as_str(),
            &path_suffix,
            ctx,
        )
        .await;
        // Header rules and transforms run after the strip and may set a
        // hop-by-hop name; the engine reads a request `Trailer` as declared
        // trailers. Strip again.
        if is_upgrade {
            headers::strip_hop_by_hop_for_upgrade(&mut outbound_headers);
        } else {
            headers::strip_hop_by_hop(&mut outbound_headers);
        }

        // 5f. ADR-0022 guard 6: the auth header was stripped before header
        //     rules and transforms; refuse a request where one of them set it
        //     (a gateway misconfiguration: 500).
        if let Some(ref state) = request_ref_state {
            secret_ref::check_auth_header_unset(
                &outbound_headers,
                &state.secret,
                upstream.id,
                route.id,
            )?;
        }

        // 6. Endpoint selection (two-tier: target-host header or round-robin).
        // The request timeout covers selection (a first selection resolves
        // the endpoints), target resolution and the exchange together, and in
        // `request_ref` mode the CredStore read before them.
        let deadline = request_ref_state.as_ref().map_or_else(
            || tokio::time::Instant::now() + self.request_timeout,
            |state| state.deadline,
        );
        let selected = tokio::time::timeout_at(
            deadline,
            self.select_endpoint(&upstream, &req_headers, &instance_uri),
        )
        .await
        .map_err(|_| DomainError::RequestTimeout {
            detail: format!(
                "selecting an endpoint timed out after {:?}",
                self.request_timeout
            ),
            instance: instance_uri.clone(),
            dispatch: Dispatch::NotSent,
        })??;
        let endpoint = &selected.endpoint;

        headers::set_host_header(&mut outbound_headers, endpoint);

        // 6b. Inject the `request_ref` key last, once the outbound headers are
        //     final (ADR-0022 guard 6). Writes refuse auth header names the
        //     gateway sets itself, such as `Host`.
        if let Some(ref state) = request_ref_state {
            state.secret.apply(&mut outbound_headers);
        }

        // 7. Check rate limits (upstream, shadowed routes, route) with
        //    scope-aware keying.
        //    A `request_ref` request was admitted before its CredStore read.
        let rate_limit_outcome = match request_ref_state {
            Some(state) => state.rate_limit,
            None => check_rate_limits(
                &self.rate_limiter,
                &upstream,
                &route,
                &shadowed_route_limits,
                client_ip,
                ctx,
                &instance_uri,
                self.metrics.as_ref(),
                &host_label,
                &path_label,
            )?,
        };

        // 8. Build upstream URL.
        // Strip the route prefix from the full path so we get:
        // endpoint + route_path + remaining_suffix.
        let route_path = route
            .match_rules
            .http
            .as_ref()
            .map_or("/", |h| h.path.as_str());
        let remaining_suffix = route_suffix(&path_suffix, route_path)?;
        let mut url = request_builder::build_upstream_url(
            endpoint,
            route_path,
            remaining_suffix,
            &query_params,
        )?;

        // 8a. Circuit breaker permit. It covers target resolution and the
        //     exchange only, so a half-open probe slot is not held through
        //     the gateway's own steps before it. Their time is not the
        //     upstream's: a deadline hit counts only if they left it the full
        //     budget. The signing phase (8c) is the gateway's too; its time
        //     is taken off the budget after it. A request pinned to one
        //     endpoint of several is not counted.
        let pinned = upstream.server.endpoints.len() > 1
            && req_headers.contains_key(TARGET_HOST_HEADER);
        let budget = deadline.saturating_duration_since(tokio::time::Instant::now());
        let mut permit = self
            .circuit_breakers
            .admit(&upstream, &instance_uri)?
            .with_budget(budget, self.request_timeout)
            .pinned_to_one_endpoint(pinned);

        // 8b. Engine target: the LB's pre-resolved address, or DNS with the
        // SSRF policy for target-host / single-endpoint selection.
        let target = match tokio::time::timeout_at(
            deadline,
            self.engine.target(&selected, &instance_uri),
        )
        .await
        {
            Ok(Ok(target)) => target,
            // A host that does not resolve is as unreachable as one that
            // refuses the connection.
            Ok(Err(TargetError::Unreachable(err))) => {
                permit.failed(&err);
                return Err(err);
            }
            // The gateway's own policy, not the upstream.
            Ok(Err(TargetError::Refused(err))) => {
                permit.ignored();
                return Err(err);
            }
            Err(_) => {
                permit.timed_out();
                return Err(DomainError::RequestTimeout {
                    detail: format!(
                        "resolving {} timed out after {:?}",
                        endpoint.host, self.request_timeout
                    ),
                    instance: instance_uri.clone(),
                    dispatch: Dispatch::NotSent,
                });
            }
        };

        let response_header_rules = upstream
            .headers
            .as_ref()
            .and_then(|hc| hc.response.as_ref());

        let pipeline = ResponsePipelineCtx {
            guard_bindings,
            transform_bindings,
            method: method.as_str(),
            path_suffix: &path_suffix,
            ctx,
            cors_config: effective_cors.as_ref(),
            origin: request_origin,
            response_header_rules,
            rate_limit_outcome,
            host: &host_label,
        };

        // Passthrough may have dropped `Connection`; the handshake needs it.
        // Set here so the signing phase sees it.
        if is_upgrade {
            outbound_headers.insert(
                http::header::CONNECTION,
                HeaderValue::from_static("upgrade"),
            );
        }

        // 8c. Signing phase of the auth plugin (ADR-0023): the last change to
        //     the URL and headers. It sees them as sent and the body (empty
        //     for a WebSocket handshake). A failure stops the request.
        //     `request_ref` mode runs no auth plugin, so it has no signing.
        //     The plugin's `sign` is timed as phase="sign", on errors and
        //     timeouts too.
        if let Some((plugin, auth)) = signer {
            // The engine's header hygiene, applied now so the signer sees what
            // the engine sends; the engine's own pass then changes nothing.
            // A handshake is validated first, as the engine does, so one it
            // would refuse is not normalised into one it accepts.
            let kind = if is_upgrade {
                sanitize::validate_websocket_upgrade(&method, &outbound_headers, true)
                    .map_err(|e| map_proxy_error(&e, &instance_uri))?;
                RequestKind::WebSocketUpgrade
            } else {
                RequestKind::Regular
            };
            sanitize::sanitize_request_headers(&mut outbound_headers, kind);
            let uri = url
                .parse::<http::Uri>()
                .map_err(|e| DomainError::DownstreamError {
                    detail: format!("invalid upstream request: {e}"),
                    instance: instance_uri.clone(),
                    dispatch: Dispatch::NotSent,
                })?;
            // The handshake goes out without a body, whatever the caller sent.
            let body = if is_upgrade {
                SigningBody::Buffered(Bytes::new())
            } else if body_stream.is_some() {
                SigningBody::Streamed
            } else {
                SigningBody::Buffered(body_bytes.clone())
            };
            // The trailing `/` is compared with the URL as built: the route
            // path gives back one the normalization removed (route `/dir/`,
            // request `/dir/`) and adds one the client did not send (route
            // `/dir/`, request `/dir`). The root `/` names no resource.
            let path_rewritten = path_rewrite.segments
                || (uri.path() != "/"
                    && path_rewrite.trailing_slash != uri.path().ends_with('/'));
            let input = signing::SigningInput {
                method: &method,
                uri,
                path_rewritten,
                port: endpoint.port,
                body,
                config: auth.config.clone().unwrap_or_default(),
                security_context: ctx.clone(),
            };
            // cancel-safe: sign_request changes the headers only after the
            // plugin's sign returns, with no await after it, and the URL is
            // replaced only on success; a timeout fails the request.
            tracing::debug!(plugin = %auth.plugin_type, "signing the request");
            let sign_start = tokio::time::Instant::now();
            let signed = tokio::time::timeout_at(
                deadline,
                signing::sign_request(plugin.as_ref(), input, &mut outbound_headers),
            )
            .await;
            let sign_time = sign_start.elapsed();
            self.metrics.record_request_duration_seconds(
                &host_label,
                &path_label,
                phase::SIGN,
                sign_time.as_secs_f64(),
            );
            // The signer's CredStore reads are the gateway's time, not the
            // upstream's: a slow store must not open the breaker.
            permit = permit.with_budget(budget.saturating_sub(sign_time), self.request_timeout);
            let signed_url = signed
                .map_err(|_| DomainError::RequestTimeout {
                    detail: format!(
                        "signing the request timed out after {:?}",
                        self.request_timeout
                    ),
                    instance: instance_uri.clone(),
                    dispatch: Dispatch::NotSent,
                })?
                .map_err(|e| {
                    auth_plugin_error(e, &instance_uri, AuthScope::new(&upstream, auth, ctx))
                })?;
            tracing::debug!(plugin = %auth.plugin_type, "request signed");
            if let Some(signed_url) = signed_url {
                url = signed_url;
            }
        }

        // 9. WebSocket upgrade: tunnel on a dedicated upstream connection,
        // only after the upstream switched protocols.
        if is_upgrade {
            return self
                .handle_websocket_upgrade(
                    &target,
                    permit,
                    deadline,
                    &method,
                    &url,
                    &outbound_headers,
                    &pipeline,
                    &instance_uri,
                )
                .await;
        }

        // 10. Forward the request through the engine.
        let upstream_result = self
            .forward(
                &target,
                permit,
                deadline,
                &method,
                &url,
                &outbound_headers,
                body_bytes,
                body_stream,
                max_body,
                &pipeline,
                &instance_uri,
            )
            .await;

        // 10b. Execute transform error plugins on upstream failures.
        match upstream_result {
            Ok(resp) => Ok(resp),
            Err(err) => {
                execute_transform_errors(
                    &self.transform_registry,
                    &pipeline.transform_bindings,
                    &err,
                    pipeline.ctx,
                )
                .await;
                Err(err)
            }
        }
        }
        .await;

        match result {
            Ok(resp) => {
                guard.set_success(resp.status().as_u16());
                Ok(guard.observe_body(resp, &request_method))
            }
            Err(err) => {
                guard.set_error(&err);
                Err(err)
            }
        }
    }
}

/// Evict idle rate-limit buckets and circuit breakers every `interval` until
/// `cancel` fires or the limiter is gone. The stream shutdown signal does not
/// stop it: a gear stop and a later start keep the same limiter and breakers.
async fn sweep_idle_state(
    limiter: Weak<RateLimiter>,
    breakers: Weak<CircuitBreakers>,
    metrics: Arc<dyn OagwMetricsPort>,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval);
    // A slow sweep should not fire a catch-up burst on the next tick.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancel.cancelled() => return,
            _ = ticker.tick() => {
                let Some(limiter) = limiter.upgrade() else {
                    return;
                };
                let evicted = limiter.evict_idle();
                let live = limiter.bucket_count();
                metrics.record_rate_limit_sweep(evicted as u64, live as u64);
                if evicted > 0 {
                    tracing::debug!(evicted, live, "evicted idle rate-limit buckets");
                }
                if let Some(breakers) = breakers.upgrade() {
                    let evicted = breakers.evict_idle();
                    if evicted > 0 {
                        tracing::debug!(evicted, live = breakers.len(), "evicted idle circuit breakers");
                    }
                }
            }
        }
    }
}

/// Private helpers for the proxy pipeline.
impl DataPlaneServiceImpl {
    /// Handle a WebSocket upgrade request.
    ///
    /// The engine opens a dedicated HTTP/1.1 connection, forwards the
    /// handshake and hands out the raw tunnel only after the upstream
    /// answered `101 Switching Protocols`. Response guards and the response
    /// pipeline run on the 101; the tunnel travels to the handler in the
    /// response extensions.
    ///
    /// The circuit breaker sees the handshake only: a 101 or another status
    /// is an answer (502-504 failures), a transport error or timeout a
    /// failure (a deadline hit as in [`UpstreamPermit::timed_out`]). What
    /// happens on the tunnel afterwards is not counted.
    #[allow(clippy::too_many_arguments)]
    async fn handle_websocket_upgrade(
        &self,
        target: &toolkit_http_proxy::Target,
        permit: UpstreamPermit,
        deadline: tokio::time::Instant,
        method: &http::Method,
        url: &str,
        outbound_headers: &HeaderMap,
        pipeline: &ResponsePipelineCtx<'_>,
        instance_uri: &str,
    ) -> Result<http::Response<Body>, DomainError> {
        let req = build_outbound_request(
            method,
            url,
            outbound_headers.clone(),
            empty_body(),
            instance_uri,
        )?;

        let upgrade_timeout = self.request_timeout;
        let outcome = match tokio::time::timeout_at(
            deadline,
            self.engine.client().upgrade(target, req),
        )
        .await
        {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(e)) => {
                let err = map_proxy_error(&e, instance_uri);
                permit.failed(&err);
                return Err(err);
            }
            Err(_) => {
                permit.timed_out();
                return Err(DomainError::RequestTimeout {
                    // No URL: its query string may carry credentials.
                    detail: format!(
                        "upstream WebSocket upgrade timed out after {upgrade_timeout:?}"
                    ),
                    instance: instance_uri.to_string(),
                    dispatch: Dispatch::MaybeSent,
                });
            }
        };

        let (response, io) = match outcome {
            UpgradeOutcome::Switched { response, io } => {
                permit.answered(response.status());
                (response, io)
            }
            UpgradeOutcome::Rejected(resp) => {
                permit.answered(resp.status());
                return Err(DomainError::ProtocolError {
                    detail: format!(
                        "upstream rejected WebSocket upgrade with status {}",
                        resp.status()
                    ),
                    instance: instance_uri.to_string(),
                    dispatch: Dispatch::MaybeSent,
                });
            }
        };
        let status = response.status();
        let mut resp_headers = response.into_parts().0.headers;
        // Upstreams must never be able to forge gateway-internal headers.
        headers::strip_internal_headers(&mut resp_headers);

        // Execute response guards on the 101.
        execute_guard_responses(
            &self.guard_registry,
            &pipeline.guard_bindings,
            status,
            &resp_headers,
            pipeline.method,
            pipeline.path_suffix,
            instance_uri,
            pipeline.ctx,
        )
        .await?;

        // Run response transforms first, then sanitize — ensures transforms
        // cannot reintroduce hop-by-hop or body-related headers that would
        // break the 101 tunnel contract.
        self.apply_response_pipeline(pipeline, status, &mut resp_headers)
            .await;
        headers::sanitize_response_headers_for_upgrade(&mut resp_headers);

        // Build the 101 response with the upstream tunnel stashed in extensions.
        let mut resp = http::Response::builder()
            .status(http::StatusCode::SWITCHING_PROTOCOLS)
            .body(Body::Empty)
            .map_err(|e| DomainError::Internal {
                message: format!("failed to build WebSocket upgrade response: {e}"),
            })?;
        *resp.headers_mut() = resp_headers;
        resp.extensions_mut()
            .insert(super::websocket::WebSocketBridgeHandle::new(
                super::websocket::WebSocketBridgeIo {
                    io: hyper_util::rt::TokioIo::new(io),
                    leftover: Bytes::new(),
                    idle_timeout: self.websocket_idle_timeout,
                    close_timeout: self.websocket_close_timeout,
                    stopped: self.stream_shutdown.subscribe(),
                    metrics: Some(self.metrics.clone()),
                    host: pipeline.host.to_string(),
                },
            ));
        Ok(resp)
    }

    /// Forward a request through the engine and run the response pipeline.
    ///
    /// Buffered bodies go out with `Content-Length`; streaming bodies are
    /// forwarded chunk by chunk with `max_body` enforced on the fly. A body
    /// that breaches the limit (413) or fails mid-stream aborts the upstream
    /// request without completing its framing.
    #[allow(clippy::too_many_arguments)]
    async fn forward(
        &self,
        target: &toolkit_http_proxy::Target,
        permit: UpstreamPermit,
        deadline: tokio::time::Instant,
        method: &http::Method,
        url: &str,
        outbound_headers: &HeaderMap,
        body_bytes: Bytes,
        body_stream: Option<BodyStream>,
        max_body: usize,
        pipeline: &ResponsePipelineCtx<'_>,
        instance_uri: &str,
    ) -> Result<http::Response<Body>, DomainError> {
        let body_state = Arc::new(StreamedBodyState::default());
        let streamed = body_stream.is_some();
        let (body, replay) = match body_stream {
            Some(stream) => (
                stream_body(limit_body_stream(stream, max_body, Arc::clone(&body_state))),
                None,
            ),
            // A buffered body can be sent again if the upstream provably did
            // not process the request (the engine decides when).
            None => (full_body(body_bytes.clone()), Some(body_bytes)),
        };
        let mut req =
            build_outbound_request(method, url, outbound_headers.clone(), body, instance_uri)?;
        if let Some(bytes) = replay {
            req.extensions_mut().insert(ReplayableBody(bytes));
        }

        let timeout = self.request_timeout;
        let outcome =
            tokio::time::timeout_at(deadline, self.engine.client().send(target, req)).await;

        // A failure on our side of the body wins over whatever the upstream
        // made of the aborted request, and says nothing about the upstream.
        if let Some(err) = body_state.failure(max_body, instance_uri) {
            return Err(err);
        }
        // Until the client has sent its whole body the exchange runs at the
        // client's pace: a client trickling its upload would otherwise hit
        // the deadline or the engine's read timeout and open the circuit
        // for every tenant. Only failing to reach the upstream counts then.
        let upload_done = !streamed || body_state.finished();
        // Recorded at the response head: an SSE or long body that fails
        // later does not count.
        let resp = match outcome {
            Ok(Ok(resp)) => {
                permit.answered(resp.status());
                resp
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    method = %method,
                    path = pipeline.path_suffix,
                    error = %e,
                    "Proxy request failed"
                );
                let err = map_proxy_error(&e, instance_uri);
                if upload_done || is_connect_failure(&e) {
                    permit.failed(&err);
                } else {
                    permit.ignored();
                }
                return Err(err);
            }
            Err(_) => {
                if upload_done {
                    permit.timed_out();
                } else {
                    permit.ignored();
                }
                return Err(DomainError::RequestTimeout {
                    // No URL: its query string may carry credentials.
                    detail: format!("upstream request timed out after {timeout:?}"),
                    instance: instance_uri.to_string(),
                    dispatch: Dispatch::MaybeSent,
                });
            }
        };

        let (parts, mut body) = resp.into_parts();
        // SSE streams may stay silent longer than the read timeout; the
        // streaming lifecycle in `finalize_response` owns their idle timeout.
        if oagw_sdk::sse::is_server_events_response(&parts.headers) {
            body.set_idle_timeout(None);
        }
        let mut resp_headers = parts.headers;
        // Upstreams must never be able to forge gateway-internal headers
        // (e.g. `x-oagw-error-source: gateway`).
        headers::strip_internal_headers(&mut resp_headers);
        self.finalize_response(
            pipeline,
            parts.status,
            resp_headers,
            into_body_stream(body),
            instance_uri.to_string(),
        )
        .await
    }
}

/// Build the outbound request handed to the engine.
fn build_outbound_request(
    method: &http::Method,
    url: &str,
    headers: HeaderMap,
    body: toolkit_http_proxy::ProxyBody,
    instance_uri: &str,
) -> Result<http::Request<toolkit_http_proxy::ProxyBody>, DomainError> {
    let mut req = http::Request::builder()
        .method(method.clone())
        .uri(url)
        .body(body)
        .map_err(|e| DomainError::DownstreamError {
            detail: format!("invalid upstream request: {e}"),
            instance: instance_uri.to_string(),
            dispatch: Dispatch::NotSent,
        })?;
    *req.headers_mut() = headers;
    Ok(req)
}

/// What happened on our side of a streamed request body.
#[derive(Default)]
struct StreamedBodyState {
    /// Total bytes when the limit was breached.
    exceeded: std::sync::OnceLock<usize>,
    /// Why the client's body stream failed.
    failed: std::sync::OnceLock<String>,
    /// The client's body stream ended cleanly.
    finished: std::sync::atomic::AtomicBool,
}

impl StreamedBodyState {
    fn finished(&self) -> bool {
        self.finished.load(std::sync::atomic::Ordering::Acquire)
    }

    fn failure(&self, max_body: usize, instance_uri: &str) -> Option<DomainError> {
        if let Some(total) = self.exceeded.get() {
            return Some(DomainError::PayloadTooLarge {
                detail: format!(
                    "streaming request body of {total} bytes exceeds maximum of {max_body} bytes"
                ),
                instance: instance_uri.to_string(),
            });
        }
        self.failed
            .get()
            .map(|reason| DomainError::DownstreamError {
                detail: format!("streaming request body failed mid-stream: {reason}"),
                instance: instance_uri.to_string(),
                dispatch: Dispatch::MaybeSent,
            })
    }
}

/// Enforce `max_body` on a streamed request body, recording failures and
/// the clean end in `state`. Empty chunks are dropped. A breach or a stream
/// error ends the stream with an error, which aborts the upstream request
/// without a terminating chunk.
fn limit_body_stream(
    stream: BodyStream,
    max_body: usize,
    state: Arc<StreamedBodyState>,
) -> impl futures_util::Stream<Item = Result<Bytes, oagw_sdk::body::BoxError>> + Send {
    let mut total: usize = 0;
    let end = Arc::clone(&state);
    let limited = stream.filter_map(move |chunk| {
        let item = match chunk {
            Ok(bytes) if bytes.is_empty() => None,
            Ok(bytes) => {
                total = total.saturating_add(bytes.len());
                if total > max_body {
                    tracing::warn!(
                        total_bytes = total,
                        max_body,
                        "streaming body exceeded max size, aborting"
                    );
                    let _ = state.exceeded.set(total);
                    Some(Err("request body exceeds the configured limit".into()))
                } else {
                    Some(Ok(bytes))
                }
            }
            Err(e) => {
                tracing::debug!(error = %e, "body stream chunk error");
                let _ = state.failed.set(format!("body stream read error: {e}"));
                Some(Err(e))
            }
        };
        std::future::ready(item)
    });
    // Polled only once the client's stream has ended. After an error item
    // the body state reports the failure first, whatever this says.
    let end = futures_util::stream::once(async move {
        end.finished
            .store(true, std::sync::atomic::Ordering::Release);
    })
    .filter_map(|()| std::future::ready(None));
    limited.chain(end)
}

/// Execute the auth plugin for the upstream and set the headers it asks for
/// (e.g. Bearer token, API key) on the outbound request. Returns the plugin
/// for its signing phase.
///
/// Only the named headers change: other lines, repeated ones and non-UTF-8
/// values included, pass through as the client sent them.
async fn execute_auth_plugin(
    auth_registry: &AuthPluginRegistry,
    upstream: &Upstream,
    auth: &crate::domain::model::AuthConfig,
    outbound_headers: &mut HeaderMap,
    ctx: &SecurityContext,
    instance_uri: &str,
) -> Result<Arc<dyn AuthPlugin>, DomainError> {
    tracing::debug!(plugin = %auth.plugin_type, "executing auth plugin");
    let plugin = auth_registry.resolve(&auth.plugin_type).map_err(|e| {
        DomainError::AuthenticationFailed {
            reason: reason::auth::PLUGIN_NOT_FOUND,
            detail: e.to_string(),
            instance: instance_uri.to_string(),
        }
    })?;
    let mut auth_ctx = AuthContext::new(auth.config.clone().unwrap_or_default(), ctx.clone());
    plugin
        .authenticate(&mut auth_ctx)
        .await
        .and_then(|()| set_auth_headers(outbound_headers, auth_ctx.set_headers))
        .map_err(|e| auth_plugin_error(e, instance_uri, AuthScope::new(upstream, auth, ctx)))?;
    tracing::debug!(plugin = %auth.plugin_type, "auth plugin succeeded");
    Ok(plugin)
}

/// Set the headers an auth plugin asked for, each replacing any line of the
/// same name, marked sensitive. A name or value that is not a valid header
/// fails the request: dropping it would send the request unauthenticated.
/// The error names the header, never the value (it carries the credential).
fn set_auth_headers(
    outbound_headers: &mut HeaderMap,
    set_headers: Vec<(String, String)>,
) -> Result<(), PluginError> {
    for (name, value) in set_headers {
        let header = http::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            PluginError::InvalidConfig(format!("auth plugin header name {name:?} is invalid"))
        })?;
        let mut value = HeaderValue::from_str(&value).map_err(|_| {
            PluginError::InvalidConfig(format!("auth plugin value for header {header} is invalid"))
        })?;
        value.set_sensitive(true);
        outbound_headers.insert(header, value);
    }
    Ok(())
}

/// The request an auth plugin ran for, as its failure log names it.
#[derive(Clone, Copy)]
struct AuthScope<'a> {
    caller_tenant_id: uuid::Uuid,
    upstream_id: uuid::Uuid,
    upstream_alias: &'a str,
    plugin_type: &'a str,
}

impl<'a> AuthScope<'a> {
    fn new(
        upstream: &'a Upstream,
        auth: &'a crate::domain::model::AuthConfig,
        ctx: &SecurityContext,
    ) -> Self {
        Self {
            caller_tenant_id: ctx.subject_tenant_id(),
            upstream_id: upstream.id,
            upstream_alias: &upstream.alias,
            plugin_type: &auth.plugin_type,
        }
    }
}

/// The one mapping of auth plugin errors, for `authenticate` and `sign`
/// alike. The detail names a config key at most, never a reference or a
/// secret; it stays in the server log. A missing secret, a credential store
/// outage, an identity provider outage and an invalid identity provider
/// response are warned here, once, with `scope`; the REST mapping does not
/// log them again.
fn auth_plugin_error(e: PluginError, instance_uri: &str, scope: AuthScope<'_>) -> DomainError {
    let instance = instance_uri.to_string();
    match e {
        PluginError::SecretNotFound { key, kind } => {
            tracing::warn!(
                caller_tenant_id = %scope.caller_tenant_id,
                upstream_id = %scope.upstream_id,
                upstream_alias = scope.upstream_alias,
                plugin = scope.plugin_type,
                config_key = %format_args!("auth.config.{key}"),
                error = kind,
                "auth plugin: credential not accessible"
            );
            DomainError::SecretNotFound {
                detail: e.to_string(),
                instance,
            }
        }
        PluginError::CredentialStoreUnavailable {
            detail,
            retry_after,
        } => {
            // `detail` names the config key being read.
            tracing::warn!(
                caller_tenant_id = %scope.caller_tenant_id,
                upstream_id = %scope.upstream_id,
                upstream_alias = scope.upstream_alias,
                plugin = scope.plugin_type,
                reason = %detail,
                "auth plugin: credential store unavailable"
            );
            DomainError::CredentialStoreUnavailable {
                detail,
                retry_after,
                instance,
            }
        }
        PluginError::IdpUnavailable(_) => {
            let detail = e.to_string();
            tracing::warn!(
                caller_tenant_id = %scope.caller_tenant_id,
                upstream_id = %scope.upstream_id,
                upstream_alias = scope.upstream_alias,
                plugin = scope.plugin_type,
                reason = %detail,
                "auth plugin: OAuth2 identity provider unavailable"
            );
            DomainError::IdpUnavailable { detail, instance }
        }
        PluginError::IdpInvalidResponse(_) => {
            let detail = e.to_string();
            tracing::warn!(
                caller_tenant_id = %scope.caller_tenant_id,
                upstream_id = %scope.upstream_id,
                upstream_alias = scope.upstream_alias,
                plugin = scope.plugin_type,
                reason = %detail,
                "auth plugin: identity provider sent an invalid response"
            );
            DomainError::IdpInvalidResponse { detail, instance }
        }
        PluginError::AuthFailed(_) | PluginError::Rejected(_) => {
            DomainError::AuthenticationFailed {
                reason: reason::auth::PLUGIN_FAILED,
                detail: e.to_string(),
                instance,
            }
        }
        PluginError::InvalidRequest {
            field: name,
            detail,
        } => DomainError::Validation {
            field: name,
            reason: field::VALIDATION,
            detail,
            instance,
        },
        PluginError::InvalidConfig(_) => {
            // The response says only `internal`; the log names the config
            // key at fault. Plugin messages never carry a secret.
            let message = format!("auth plugin misconfigured: {e}");
            tracing::warn!(
                caller_tenant_id = %scope.caller_tenant_id,
                upstream_id = %scope.upstream_id,
                upstream_alias = scope.upstream_alias,
                plugin = scope.plugin_type,
                reason = %message,
                "auth plugin misconfigured"
            );
            DomainError::Internal { message }
        }
        PluginError::Internal(_) => {
            // A stored secret the header cannot carry, or an unexpected
            // store error: logged as in `request_ref` mode, so an operator
            // can find it. The detail names the config key, never the value.
            let detail = e.to_string();
            tracing::warn!(
                caller_tenant_id = %scope.caller_tenant_id,
                upstream_id = %scope.upstream_id,
                upstream_alias = scope.upstream_alias,
                plugin = scope.plugin_type,
                reason = %detail,
                "auth plugin failed"
            );
            DomainError::AuthenticationFailed {
                reason: reason::auth::PLUGIN_INTERNAL,
                detail,
                instance,
            }
        }
    }
}

/// Execute guard plugins on the request, returning the first rejection.
///
/// Guards use fail-hard semantics: the first rejection or error terminates the
/// pipeline immediately.
async fn execute_guard_requests(
    guard_registry: &GuardPluginRegistry,
    guard_bindings: &[&crate::domain::model::PluginBinding],
    inbound_headers: &HeaderMap,
    method: &str,
    path_suffix: &str,
    ctx: &SecurityContext,
    instance_uri: &str,
) -> Result<(), DomainError> {
    let guard_headers = headers::header_map_to_vec(inbound_headers);
    for binding in guard_bindings {
        let guard =
            guard_registry
                .resolve(&binding.plugin_ref)
                .map_err(|e| DomainError::Internal {
                    message: format!(
                        "guard plugin '{}' resolution failed: {e}",
                        binding.plugin_ref
                    ),
                })?;

        let guard_ctx = GuardContext {
            method: method.to_string(),
            path: path_suffix.to_string(),
            status: None,
            headers: guard_headers.clone(),
            config: binding.config.clone(),
            security_context: ctx.clone(),
        };

        match guard.guard_request(&guard_ctx).await {
            Ok(GuardDecision::Allow) => {}
            Ok(GuardDecision::Reject {
                status,
                error_code,
                detail,
                resource_id,
            }) => {
                return Err(DomainError::GuardRejected {
                    status,
                    error_code,
                    detail,
                    instance: instance_uri.to_string(),
                    resource_id,
                });
            }
            Err(e) => {
                return Err(DomainError::Internal {
                    message: format!("guard plugin error: {e}"),
                });
            }
        }
    }
    Ok(())
}

/// Execute transform plugins on the request (on_request phase).
///
/// Placed after header rules so transforms have the final word on outbound
/// headers. Errors are logged and skipped — transforms use log-and-continue
/// semantics so a single misbehaving transform cannot block the pipeline.
async fn execute_transform_requests(
    transform_registry: &TransformPluginRegistry,
    transform_bindings: &[&crate::domain::model::PluginBinding],
    outbound_headers: &mut HeaderMap,
    query_params: &mut Vec<(String, String)>,
    method: &str,
    path_suffix: &str,
    ctx: &SecurityContext,
) {
    if transform_bindings.is_empty() {
        return;
    }

    let original_headers = headers::header_map_to_vec(outbound_headers);
    let mut transform_headers = original_headers.clone();
    let mut transform_query: Vec<(String, String)> = query_params.clone();

    for binding in transform_bindings {
        let mut transform_ctx = TransformRequestContext {
            method: method.to_string(),
            path: path_suffix.to_string(),
            query: transform_query.clone(),
            headers: transform_headers.clone(),
            config: binding.config.clone(),
            security_context: ctx.clone(),
        };
        match transform_registry.resolve(&binding.plugin_ref) {
            Ok(transform) => match transform.on_request(&mut transform_ctx).await {
                Ok(()) => {
                    transform_headers = transform_ctx.headers;
                    transform_query = transform_ctx.query;
                }
                Err(e) => {
                    tracing::warn!(
                        plugin = %binding.plugin_ref,
                        error = %e,
                        "transform on_request failed, continuing"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    plugin = %binding.plugin_ref,
                    error = %e,
                    "transform plugin resolution failed, continuing"
                );
                continue;
            }
        }
    }

    // Apply only what the transforms changed: other lines, credentials set by
    // the auth plugin included, keep their bytes and sensitive flag.
    headers::apply_header_edits(outbound_headers, &original_headers, &transform_headers);
    *query_params = transform_query;
}

/// Enforce CORS origin and method constraints for actual (non-preflight) requests.
///
/// Validates the `Origin` header against the upstream's CORS config and rejects
/// disallowed origins/methods before the request reaches the upstream.
fn enforce_cors_origin(
    cors_config: Option<&crate::domain::model::CorsConfig>,
    origin: Option<&str>,
    method: &str,
    instance_uri: &str,
) -> Result<(), DomainError> {
    if let Some(cors_config) = cors_config
        && cors_config.enabled
        && let Some(origin) = origin
    {
        if !crate::domain::cors::is_origin_allowed(cors_config, origin) {
            return Err(DomainError::CorsOriginNotAllowed {
                origin: origin.to_string(),
                instance: instance_uri.to_string(),
            });
        }
        if !crate::domain::cors::is_method_allowed(cors_config, method) {
            return Err(DomainError::CorsMethodNotAllowed {
                method: method.to_string(),
                instance: instance_uri.to_string(),
            });
        }
    }
    Ok(())
}

/// What a `request_ref` request carries through the pipeline (ADR-0022): the
/// rate admission taken before its CredStore read, the resolved key, and the
/// deadline that started at the read. Built together after the read, absent
/// in `config` mode, so the three cannot get out of step.
struct RequestRefState {
    /// The upstream and route admission from step 5.
    rate_limit: Option<(RateLimitOutcome, bool)>,
    /// The resolved key, checked at step 5f and injected at step 6b.
    secret: secret_ref::InjectedSecret,
    /// The request deadline, started at the CredStore read.
    deadline: tokio::time::Instant,
}

/// Check rate limits for both upstream and route, returning the tightest outcome.
///
/// Buckets are checked narrowest first: the route's bucket (the route's own
/// limit), the bucket of each shadowed ancestor route with an enforced limit
/// (`shadowed` is root-first and is walked closest-first; keyed and scoped as
/// if that route had matched), the upstream's scoped bucket (the
/// tenant-merged upstream limit), then the shared budget's pool bucket when
/// the limit draws on one. Each passed bucket stays decremented, as there is
/// no refund, so a wider bucket is charged only once every narrower one
/// admitted the request: a tenant calling a route it has used up does not
/// drain its upstream bucket or the pool its sibling tenants draw on. A
/// rejection reports the rejecting bucket's numbers and `Retry-After`.
///
/// `client_ip` is the address the API gateway resolved (`ClientIp`); `None`
/// keys `scope: ip` limits on the shared `unknown` bucket.
///
/// Side effect: updates `rate_limit_usage_ratio` and (on rejection)
/// `rate_limit_exceeded_total` metrics via the injected port, one series
/// per [`RateLimitBucket`]; the shadowed routes' series gets the highest
/// ratio among them.
#[allow(clippy::too_many_arguments)]
fn check_rate_limits(
    rate_limiter: &RateLimiter,
    upstream: &Upstream,
    route: &crate::domain::model::Route,
    shadowed: &[ShadowedRouteLimit],
    client_ip: Option<std::net::IpAddr>,
    ctx: &SecurityContext,
    instance_uri: &str,
    metrics: &dyn OagwMetricsPort,
    host_label: &str,
    path_label: &str,
) -> Result<Option<(RateLimitOutcome, bool)>, DomainError> {
    let mut outcome: Option<(RateLimitOutcome, bool)> = None;
    let tenant_id = ctx.subject_tenant_id();
    let subject_id = ctx.subject_id();

    // Consume a token; the bucket's usage ratio when it admits the request.
    let mut consume = |key: &str,
                       rl: &RateLimitConfig,
                       bucket: RateLimitBucket|
     -> Result<Option<f64>, DomainError> {
        let result = rate_limiter
            .try_consume(key, rl, instance_uri)
            .inspect_err(|e| {
                if matches!(e, DomainError::RateLimitExceeded { .. }) {
                    metrics.record_rate_limit_exceeded(host_label, path_label, bucket);
                    metrics.record_rate_limit_usage_ratio(host_label, path_label, bucket, 1.0);
                }
            })?;
        let ratio = (result.limit > 0)
            .then(|| result.limit.saturating_sub(result.remaining) as f64 / result.limit as f64);
        // The tighter bucket's numbers are reported; on a tie the earlier
        // one stays. Headers are sent if any checked limit enables them.
        outcome = match outcome {
            Some((existing, shown)) if existing.remaining <= result.remaining => {
                Some((existing, shown || rl.response_headers))
            }
            previous => Some((
                result,
                rl.response_headers || previous.is_some_and(|(_, shown)| shown),
            )),
        };
        Ok(ratio)
    };
    let record_usage = |bucket: RateLimitBucket, ratio: Option<f64>| {
        if let Some(ratio) = ratio {
            metrics.record_rate_limit_usage_ratio(host_label, path_label, bucket, ratio);
        }
    };
    let route_key = |route_id: &uuid::Uuid, rl: &RateLimitConfig| {
        build_rate_limit_key(&RateLimitKeyContext {
            resource: RateLimitResource::Route,
            resource_id: route_id,
            limit_owner: None,
            scope: &rl.scope,
            tenant_id: &tenant_id,
            subject_id: &subject_id,
            route_id,
            client_ip,
            window: &rl.sustained.window,
        })
    };

    if let Some(ref rl) = route.rate_limit {
        let ratio = consume(&route_key(&route.id, rl), rl, RateLimitBucket::Route)?;
        record_usage(RateLimitBucket::Route, ratio);
    }
    // The shadowed routes share one series: it shows the one closest to its
    // limit, not the last one checked.
    let mut shadowed_max: Option<f64> = None;
    for s in shadowed.iter().rev() {
        let ratio = consume(
            &route_key(&s.route_id, &s.rate_limit),
            &s.rate_limit,
            RateLimitBucket::ShadowedRoute,
        )?;
        shadowed_max = match (shadowed_max, ratio) {
            (Some(max), Some(ratio)) => Some(max.max(ratio)),
            (max, ratio) => max.or(ratio),
        };
    }
    record_usage(RateLimitBucket::ShadowedRoute, shadowed_max);

    if let Some(ref rl) = upstream.rate_limit {
        // A shared budget keys the scoped bucket on the pool owner, so
        // children binding the alias themselves count as one upstream.
        let resource_id = rl
            .shared_pool
            .as_ref()
            .map_or(&upstream.id, |p| &p.owner_id);
        // Tenants on one upstream (or one shared pool) can resolve different
        // limits (another tenant's `private` one is not theirs): the bucket
        // names whose limit it is when that is not the key's resource, else
        // one tenant's requests would consume another limit's bucket. Tenants
        // that inherit the pool owner's limit keep sharing its bucket.
        let limit_owner = rl
            .limit_owner_id
            .as_ref()
            .filter(|owner| *owner != resource_id);
        let key = build_rate_limit_key(&RateLimitKeyContext {
            resource: RateLimitResource::Upstream,
            resource_id,
            limit_owner,
            scope: &rl.scope,
            tenant_id: &tenant_id,
            subject_id: &subject_id,
            route_id: &route.id,
            client_ip,
            window: &rl.sustained.window,
        });
        let ratio = consume(&key, rl, RateLimitBucket::Upstream)?;
        record_usage(RateLimitBucket::Upstream, ratio);

        if let Some(ref pool) = rl.shared_pool {
            let ratio = consume(
                &build_pool_key(pool),
                &pool_config(pool, rl),
                RateLimitBucket::Pool,
            )?;
            record_usage(RateLimitBucket::Pool, ratio);
        }
    }

    Ok(outcome)
}

/// Collect plugin bindings from the effective upstream, filtered by a type predicate.
///
/// The upstream already contains merged route plugins (via `compute_effective_config`),
/// so only the upstream's plugin list is consulted.
fn collect_plugin_bindings(
    upstream: &Upstream,
    predicate: fn(&str) -> bool,
) -> Vec<&crate::domain::model::PluginBinding> {
    upstream
        .plugins
        .as_ref()
        .into_iter()
        .flat_map(|pc| &pc.items)
        .filter(|b| predicate(&b.plugin_ref))
        .collect()
}

/// Execute `guard_response` for all guard bindings, returning the first rejection.
///
/// Guards use fail-hard semantics: the first rejection or error terminates the
/// pipeline. This is intentional — response guards enforce hard policies such as
/// blocking unexpected content types from compromised upstreams.
#[allow(clippy::too_many_arguments)]
async fn execute_guard_responses(
    guard_registry: &GuardPluginRegistry,
    guard_bindings: &[&crate::domain::model::PluginBinding],
    resp_status: http::StatusCode,
    resp_headers: &HeaderMap,
    method: &str,
    path: &str,
    instance_uri: &str,
    security_context: &SecurityContext,
) -> Result<(), DomainError> {
    let resp_header_map = headers::header_map_to_vec(resp_headers);

    for binding in guard_bindings {
        let guard =
            guard_registry
                .resolve(&binding.plugin_ref)
                .map_err(|e| DomainError::Internal {
                    message: format!(
                        "guard plugin '{}' resolution failed: {e}",
                        binding.plugin_ref
                    ),
                })?;

        let guard_ctx = GuardContext {
            method: method.to_string(),
            path: path.to_string(),
            status: Some(resp_status.as_u16()),
            headers: resp_header_map.clone(),
            config: binding.config.clone(),
            security_context: security_context.clone(),
        };

        match guard.guard_response(&guard_ctx).await {
            Ok(GuardDecision::Allow) => {}
            Ok(GuardDecision::Reject {
                status,
                error_code,
                detail,
                resource_id,
            }) => {
                return Err(DomainError::GuardRejected {
                    status,
                    error_code,
                    detail,
                    instance: instance_uri.to_string(),
                    resource_id,
                });
            }
            Err(e) => {
                return Err(DomainError::Internal {
                    message: format!("guard plugin error: {e}"),
                });
            }
        }
    }
    Ok(())
}

/// Execute `on_response` for all transform bindings, logging errors without aborting.
///
/// Unlike guard execution, transform errors are logged and skipped — a single
/// misbehaving transform must not block the response pipeline.
async fn execute_transform_responses(
    transform_registry: &TransformPluginRegistry,
    transform_bindings: &[&crate::domain::model::PluginBinding],
    resp_status: http::StatusCode,
    resp_headers: &mut HeaderMap,
    security_context: &SecurityContext,
) {
    if transform_bindings.is_empty() {
        return;
    }

    let original_headers = headers::header_map_to_vec(resp_headers);
    let mut header_map = original_headers.clone();

    for binding in transform_bindings {
        let mut transform_ctx = TransformResponseContext {
            status: resp_status.as_u16(),
            headers: header_map.clone(),
            config: binding.config.clone(),
            security_context: security_context.clone(),
        };

        match transform_registry.resolve(&binding.plugin_ref) {
            Ok(transform) => match transform.on_response(&mut transform_ctx).await {
                Ok(()) => {
                    header_map = transform_ctx.headers;
                }
                Err(e) => {
                    tracing::warn!(
                        plugin = %binding.plugin_ref,
                        error = %e,
                        "transform on_response failed, continuing"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    plugin = %binding.plugin_ref,
                    error = %e,
                    "transform plugin resolution failed, continuing"
                );
                continue;
            }
        }
    }

    headers::apply_header_edits(resp_headers, &original_headers, &header_map);
}

/// Per-request plugin pipeline state shared across the streaming and buffered
/// response paths.
struct ResponsePipelineCtx<'a> {
    guard_bindings: Vec<&'a crate::domain::model::PluginBinding>,
    transform_bindings: Vec<&'a crate::domain::model::PluginBinding>,
    method: &'a str,
    path_suffix: &'a str,
    ctx: &'a SecurityContext,
    cors_config: Option<&'a crate::domain::model::CorsConfig>,
    origin: Option<String>,
    response_header_rules: Option<&'a ResponseHeaderRules>,
    rate_limit_outcome: Option<(RateLimitOutcome, bool)>,
    /// Upstream alias — `host` label used by WebSocket session metrics.
    host: &'a str,
}

/// Execute `on_error` for all transform bindings, logging errors without aborting.
///
/// Called when the upstream exchange fails (timeout, downstream error, guard
/// rejection, etc.). Transforms can enrich error details or inject diagnostic
/// headers. The original `DomainError` is not modified — transforms operate on
/// a snapshot via `TransformErrorContext`.
async fn execute_transform_errors(
    transform_registry: &TransformPluginRegistry,
    transform_bindings: &[&crate::domain::model::PluginBinding],
    err: &DomainError,
    security_context: &SecurityContext,
) {
    if transform_bindings.is_empty() {
        return;
    }

    let status = domain_error_status(err);
    let error_type = domain_error_type_name(err);

    for binding in transform_bindings {
        let mut transform_ctx = TransformErrorContext {
            error_type: error_type.to_string(),
            status,
            detail: err.to_string(),
            config: binding.config.clone(),
            security_context: security_context.clone(),
        };

        match transform_registry.resolve(&binding.plugin_ref) {
            Ok(transform) => {
                if let Err(e) = transform.on_error(&mut transform_ctx).await {
                    tracing::warn!(
                        plugin = %binding.plugin_ref,
                        error = %e,
                        "transform on_error failed, continuing"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    plugin = %binding.plugin_ref,
                    error = %e,
                    "transform plugin resolution failed, continuing"
                );
                continue;
            }
        }
    }
}

/// Map a `DomainError` to its HTTP status code by delegating to the canonical
/// wire mapping in [`crate::api::rest::error`]. Keeps the status visible to
/// transform plugins (`TransformErrorContext.status`) consistent with the
/// status the client will actually observe on the wire.
fn domain_error_status(err: &DomainError) -> u16 {
    toolkit_canonical_errors::CanonicalError::from(err.clone()).status_code()
}

/// Short discriminant name for a `DomainError` variant.
fn domain_error_type_name(err: &DomainError) -> &'static str {
    match err {
        DomainError::Validation { .. } => "ValidationError",
        DomainError::Conflict { .. } => "Conflict",
        DomainError::RegistryManaged { .. } => "RegistryManaged",
        DomainError::MissingTargetHost { .. } => "MissingTargetHost",
        DomainError::InvalidTargetHost { .. } => "InvalidTargetHost",
        DomainError::UnknownTargetHost { .. } => "UnknownTargetHost",
        DomainError::AuthenticationFailed { .. } => "AuthenticationFailed",
        DomainError::NotFound { .. } => "NotFound",
        DomainError::PayloadTooLarge { .. } => "PayloadTooLarge",
        DomainError::RateLimitExceeded { .. } => "RateLimitExceeded",
        DomainError::SecretNotFound { .. } => "SecretNotFound",
        DomainError::SecretRefNotAccessible { .. } => "SecretRefNotAccessible",
        DomainError::CredentialStoreUnavailable { .. } => "CredentialStoreUnavailable",
        DomainError::IdpUnavailable { .. } => "IdpUnavailable",
        DomainError::IdpInvalidResponse { .. } => "IdpInvalidResponse",
        DomainError::DownstreamError { .. } => "DownstreamError",
        DomainError::ProtocolError { .. } => "ProtocolError",
        DomainError::UpstreamDisabled { .. } => "UpstreamDisabled",
        DomainError::ConnectionTimeout { .. } => "ConnectionTimeout",
        DomainError::RequestTimeout { .. } => "RequestTimeout",
        DomainError::Internal { .. } => "Internal",
        DomainError::GuardRejected { .. } => "GuardRejected",
        DomainError::CorsOriginNotAllowed { .. } => "CorsOriginNotAllowed",
        DomainError::CorsMethodNotAllowed { .. } => "CorsMethodNotAllowed",
        DomainError::StreamAborted { .. } => "StreamAborted",
        DomainError::LinkUnavailable { .. } => "LinkUnavailable",
        DomainError::CircuitBreakerOpen { .. } => "CircuitBreakerOpen",
        DomainError::IdleTimeout { .. } => "IdleTimeout",
        DomainError::PluginNotFound { .. } => "PluginNotFound",
        DomainError::PluginInUse { .. } => "PluginInUse",
        DomainError::Forbidden { .. } => "Forbidden",
    }
}

/// Build the final proxy response: extract error source, sanitize headers,
/// assemble the `http::Response<Body>`.
fn build_proxy_response(
    status: http::StatusCode,
    mut resp_headers: HeaderMap,
    body_stream: BodyStream,
    instance_uri: String,
) -> Result<http::Response<Body>, DomainError> {
    let error_source = headers::extract_error_source(&resp_headers);
    headers::sanitize_response_headers(&mut resp_headers);

    let mut resp = http::Response::builder()
        .status(status)
        .body(Body::Stream(body_stream))
        .map_err(|e| DomainError::DownstreamError {
            detail: format!("failed to build response: {e}"),
            instance: instance_uri,
            dispatch: Dispatch::MaybeSent,
        })?;
    *resp.headers_mut() = resp_headers;
    resp.extensions_mut().insert(error_source);
    Ok(resp)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::domain::model::{Endpoint, ManagedBy, Scheme, Server, Upstream};
    use crate::domain::proxy_path::normalize_percent_encoding;
    use crate::domain::proxy_path::resolve_dots_for_tests as normalize_path;
    use crate::domain::services::{EndpointSelector, SelectionError};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    #[test]
    fn normalize_collapses_double_slashes() {
        assert_eq!(normalize_path("/alias//v1//chat"), "/alias/v1/chat");
    }

    fn parse_path(path: &str) -> Result<ParsedRequest, DomainError> {
        let req = http::Request::builder()
            .uri(path)
            .body(Body::Empty)
            .expect("request");
        parse_proxy_request(req, 1024)
    }

    #[test]
    fn encoded_dot_segments_are_rejected() {
        for path in [
            "/alias/v1/%2e%2e/admin",
            "/alias/v1/%2E%2E/admin",
            "/alias/v1/.%2e/admin",
            "/alias/v1/%2e./admin",
            "/alias/v1/%2e/admin",
            "/alias/v1/..%2fadmin",
            "/alias/v1/..%2Fadmin",
            "/alias/v1/..%5cadmin",
            "/alias/v1/..\\admin",
            "/alias/v1/x%2f..%2fadmin",
            // A `;` path parameter Tomcat and Jetty strip before resolving.
            "/alias/v1/..;/admin",
            "/alias/v1/%2e%2e;x/admin",
            "/alias/v1/..%3b/admin",
            // Encoded twice and three times; four is too deep to follow.
            "/alias/v1/%252e%252e/admin",
            "/alias/v1/%25252e/admin",
            "/alias/v1/%2525252e/admin",
            // Overlong UTF-8 for `.`, and percent-encoding that is not UTF-8.
            "/alias/v1/%c0%ae%c0%ae/admin",
            "/alias/v1/%ff",
            // Unicode forms an upstream may fold to `..` or `/`.
            "/alias/v1/%EF%BC%8E%EF%BC%8E/admin",
            "/alias/v1/%E2%80%A5/admin",
            "/alias/v1/..%EF%BC%8Fadmin",
            "/alias/v1/public/%EF%B8%B0/admin",
            "/alias/v1/public/..%EF%B9%A8admin",
            // A query or fragment marker after a decoded `..`.
            "/alias/v1/public/..%3F",
            "/alias/v1/public/..%23x",
            // A path parameter, raw or encoded.
            "/alias/v1/admin;x/secret",
            "/alias/v1/admin%3bx/secret",
            // A dot-segment padded with NUL or a space.
            "/alias/v1/..%00/admin",
            "/alias/v1/..%20/admin",
            "/alias/v1/%2e%2e%09/admin",
            // A stray `%` that normalization would turn into a new escape.
            "/alias/v1/ch%%36%31t",
            "/alias/v1/a%",
            "/alias/v1/a%2",
        ] {
            match parse_path(path) {
                Err(DomainError::Validation { reason, field, .. }) => {
                    assert_eq!(reason, field::INVALID_PROXY_PATH, "{path}");
                    assert_eq!(field, "path", "{path}");
                }
                Err(other) => panic!("{path}: unexpected error {other:?}"),
                Ok(_) => panic!("{path}: accepted"),
            }
        }
    }

    #[test]
    fn encoded_slash_and_literal_dots_are_kept() {
        // An encoded `/` inside a segment is forwarded encoded.
        assert_eq!(
            parse_path("/alias/v1/a%2Fb").unwrap().path_suffix,
            "/v1/a%2Fb"
        );
        assert_eq!(
            parse_path("/alias/v1/a%252Fb").unwrap().path_suffix,
            "/v1/a%252Fb"
        );
        // Dots that are not a whole segment stay.
        assert_eq!(
            parse_path("/alias/v1/a..b/.x").unwrap().path_suffix,
            "/v1/a..b/.x"
        );
        assert_eq!(
            parse_path("/alias/v1/%2e%2ex").unwrap().path_suffix,
            "/v1/..x"
        );
        // Literal dot-segments are resolved, not rejected.
        assert_eq!(parse_path("/alias/v1/../v2").unwrap().path_suffix, "/v2");
        // A literal `%` stays.
        assert_eq!(
            parse_path("/alias/v1/100%25").unwrap().path_suffix,
            "/v1/100%25"
        );
    }

    /// A segment encoded three times is followed to its end; one encoded
    /// four times is refused as too deep, which tells the limit apart from a
    /// dot-segment.
    #[test]
    fn decode_depth_boundary() {
        assert_eq!(
            parse_path("/alias/v1/a%25252Fb").unwrap().path_suffix,
            "/v1/a%25252Fb"
        );
        match parse_path("/alias/v1/a%2525252Fb") {
            Err(DomainError::Validation { detail, .. }) => {
                assert!(detail.contains("too many times"), "{detail}");
            }
            Err(other) => panic!("expected a depth rejection, got {other:?}"),
            Ok(parsed) => panic!("accepted as {}", parsed.path_suffix),
        }
    }

    /// The suffix after a route stored with escapes is cut where matching
    /// put the prefix's end.
    #[test]
    fn route_suffix_follows_the_normalized_prefix() {
        assert_eq!(route_suffix("/v1/~user/x", "/v1/%7euser").unwrap(), "/x");
        assert_eq!(route_suffix("/v1/~user", "/v1/%7euser").unwrap(), "");
        assert!(matches!(
            route_suffix("/v2/x", "/v1"),
            Err(DomainError::Internal { .. })
        ));
    }

    /// Route matching sees encoded unreserved characters decoded, so
    /// `/v1/ch%61t` is `/v1/chat`, not a request for a broader route.
    #[test]
    fn encoded_unreserved_characters_are_decoded() {
        assert_eq!(
            parse_path("/alias/v1/ch%61t").unwrap().path_suffix,
            "/v1/chat"
        );
        assert_eq!(
            parse_path("/alias/%7Euser/a%2db").unwrap().path_suffix,
            "/~user/a-b"
        );
        // Other escapes stay encoded, with uppercase hex.
        assert_eq!(
            parse_path("/alias/v1/a%2fb%3a").unwrap().path_suffix,
            "/v1/a%2Fb%3A"
        );
        assert_eq!(
            parse_path("/alias/v1/%e2%82%ac").unwrap().path_suffix,
            "/v1/%E2%82%AC"
        );
    }

    proptest::proptest! {
        /// Whatever pieces a segment is built from, a path the parser accepts
        /// never decodes, once or twice, to a `.` or `..` segment.
        #[test]
        fn accepted_paths_have_no_hidden_dot_segments(
            pieces in proptest::collection::vec(
                proptest::sample::select(vec![
                    ".", "%2e", "%2E", "%252e", "%2f", "%2F", "%5c", "/", ";", "%3b",
                    "a", "b", "%25", "%61",
                ]),
                0..8,
            ),
        ) {
            let path = format!("/alias/v1/{}", pieces.concat());
            if let Ok(parsed) = parse_path(&path) {
                let once = percent_encoding::percent_decode_str(&parsed.path_suffix)
                    .decode_utf8_lossy()
                    .into_owned();
                let twice = percent_encoding::percent_decode_str(&once)
                    .decode_utf8_lossy()
                    .into_owned();
                for decoded in [once, twice] {
                    // Independent of `is_dot_segment`: a server splits on `/`
                    // and `\\`, and drops a `;` path parameter.
                    let hidden = decoded
                        .split(['/', '\\'])
                        .map(|piece| match piece.find([';', '?', '#']) {
                            Some(at) => &piece[..at],
                            None => piece,
                        })
                        .any(|piece| piece == "." || piece == "..");
                    proptest::prop_assert!(
                        !hidden,
                        "{} accepted as {}", path, parsed.path_suffix
                    );
                }
            }
        }

        /// Normalizing is idempotent and keeps the decoded path: a second
        /// pass cannot reveal a different route.
        #[test]
        fn normalization_is_idempotent(
            pieces in proptest::collection::vec(
                proptest::sample::select(vec![
                    "%", "%3", "%36", "%31", "%61", "%2f", "%2F", "%7e", "a", "t", "/", "%25",
                ]),
                0..10,
            ),
        ) {
            let path = format!("/alias/v1/{}", pieces.concat());
            if let Ok(parsed) = parse_path(&path) {
                let again = normalize_percent_encoding(&parsed.path_suffix);
                proptest::prop_assert_eq!(&again, &parsed.path_suffix);
                let decode = |p: &str| {
                    percent_encoding::percent_decode_str(p)
                        .decode_utf8_lossy()
                        .into_owned()
                };
                let raw_suffix = path.trim_start_matches("/alias");
                proptest::prop_assert_eq!(
                    normalize_path(&decode(&parsed.path_suffix)),
                    normalize_path(&decode(raw_suffix))
                );
            }
        }
    }

    #[test]
    fn normalize_resolves_dot_dot() {
        assert_eq!(normalize_path("/alias/../admin/secret"), "/admin/secret");
    }

    #[test]
    fn normalize_clamps_above_root() {
        assert_eq!(normalize_path("/alias/../../etc/passwd"), "/etc/passwd");
    }

    #[test]
    fn normalize_resolves_single_dot() {
        assert_eq!(normalize_path("/alias/./v1/chat"), "/alias/v1/chat");
    }

    #[test]
    fn normalize_preserves_clean_path() {
        assert_eq!(normalize_path("/alias/v1/chat"), "/alias/v1/chat");
    }

    // -----------------------------------------------------------------------
    // select_endpoint() unit tests
    // -----------------------------------------------------------------------

    fn ep(host: &str, port: u16) -> Endpoint {
        Endpoint {
            scheme: Scheme::Https,
            host: host.to_string(),
            port,
        }
    }

    fn upstream_with(endpoints: Vec<Endpoint>) -> Upstream {
        Upstream {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            alias: "test".to_string(),
            server: Server { endpoints },
            protocol: "http".to_string(),
            enabled: true,
            auth: None,
            headers: None,
            plugins: None,
            rate_limit: None,
            circuit_breaker: None,
            cors: None,
            tags: vec![],
            managed_by: ManagedBy::Api,
        }
    }

    /// Mock BackendSelector that returns endpoints[call_count % endpoints.len()].
    struct MockSelector {
        call_count: AtomicUsize,
    }

    impl MockSelector {
        fn new() -> Self {
            Self {
                call_count: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::Relaxed)
        }
    }

    #[async_trait]
    impl EndpointSelector for MockSelector {
        async fn select(
            &self,
            _upstream_id: Uuid,
            endpoints: &[Endpoint],
        ) -> Result<SelectedEndpoint, SelectionError> {
            let idx = self.call_count.fetch_add(1, Ordering::Relaxed) % endpoints.len();
            Ok(SelectedEndpoint {
                endpoint: endpoints[idx].clone(),
                resolved_addr: None,
            })
        }

        fn invalidate(&self, _upstream_id: Uuid) {}
    }

    /// Build a minimal `DataPlaneServiceImpl` with the given `BackendSelector`.
    fn build_svc(selector: Arc<dyn EndpointSelector>) -> DataPlaneServiceImpl {
        build_svc_with_metrics(selector, Arc::new(crate::domain::ports::NoopMetrics))
    }

    fn build_svc_with_metrics(
        selector: Arc<dyn EndpointSelector>,
        metrics: Arc<dyn OagwMetricsPort>,
    ) -> DataPlaneServiceImpl {
        build_svc_resolving(selector, metrics, None)
    }

    /// Like [`build_svc_with_metrics`], with a CP whose `resolve_proxy_target`
    /// returns `resolved`, or "not found" when it is `None`.
    fn build_svc_resolving(
        selector: Arc<dyn EndpointSelector>,
        metrics: Arc<dyn OagwMetricsPort>,
        resolved: Option<(Upstream, crate::domain::model::Route)>,
    ) -> DataPlaneServiceImpl {
        build_svc_with_decision(selector, metrics, resolved, true)
    }

    /// Like [`build_svc_resolving`], with every authorization answered by
    /// `allow`.
    fn build_svc_with_decision(
        selector: Arc<dyn EndpointSelector>,
        metrics: Arc<dyn OagwMetricsPort>,
        resolved: Option<(Upstream, crate::domain::model::Route)>,
        allow: bool,
    ) -> DataPlaneServiceImpl {
        build_svc_with_credstore(
            selector,
            metrics,
            resolved,
            allow,
            Arc::new(credstore_sdk::test_util::MockCredStoreClient::empty()),
        )
    }

    /// Like [`build_svc_with_decision`], with the credential store the auth
    /// plugins read from.
    fn build_svc_with_credstore(
        selector: Arc<dyn EndpointSelector>,
        metrics: Arc<dyn OagwMetricsPort>,
        resolved: Option<(Upstream, crate::domain::model::Route)>,
        allow: bool,
        credstore: Arc<dyn credstore_sdk::CredStoreClientV1>,
    ) -> DataPlaneServiceImpl {
        use authz_resolver_sdk::{
            AuthZResolverApi, EvaluationRequest, EvaluationResponse, EvaluationResponseContext,
            PolicyEnforcer,
        };
        use toolkit_canonical_errors::CanonicalError;
        use toolkit_security::{PlatformSecurityContext, SecurityContext};

        struct FixedAuthZ(bool);
        #[async_trait]
        impl AuthZResolverApi for FixedAuthZ {
            async fn evaluate(
                &self,
                _ctx: PlatformSecurityContext,
                _request: EvaluationRequest,
            ) -> Result<EvaluationResponse, CanonicalError> {
                Ok(EvaluationResponse {
                    decision: self.0,
                    context: EvaluationResponseContext {
                        constraints: Vec::new(),
                        deny_reason: None,
                    },
                })
            }
        }

        let policy_enforcer = PolicyEnforcer::new(Arc::new(FixedAuthZ(allow)));

        // Minimal CP — only the proxy lookup is implemented.
        use crate::domain::error::DomainError;
        use crate::domain::model::*;
        use crate::domain::services::ControlPlaneService;

        struct NoopCp(Option<(Upstream, Route)>);
        #[async_trait]
        impl ControlPlaneService for NoopCp {
            async fn create_upstream(
                &self,
                _: &SecurityContext,
                _: CreateUpstreamRequest,
            ) -> Result<Upstream, DomainError> {
                unimplemented!()
            }
            async fn get_upstream(
                &self,
                _: &SecurityContext,
                _: Uuid,
            ) -> Result<Upstream, DomainError> {
                unimplemented!()
            }
            async fn list_upstreams(
                &self,
                _: &SecurityContext,
                _: &ListQuery,
            ) -> Result<Vec<Upstream>, DomainError> {
                unimplemented!()
            }
            async fn update_upstream(
                &self,
                _: &SecurityContext,
                _: Uuid,
                _: UpdateUpstreamRequest,
            ) -> Result<Upstream, DomainError> {
                unimplemented!()
            }
            async fn delete_upstream(
                &self,
                _: &SecurityContext,
                _: Uuid,
            ) -> Result<(), DomainError> {
                unimplemented!()
            }
            async fn create_route(
                &self,
                _: &SecurityContext,
                _: CreateRouteRequest,
            ) -> Result<Route, DomainError> {
                unimplemented!()
            }
            async fn get_route(&self, _: &SecurityContext, _: Uuid) -> Result<Route, DomainError> {
                unimplemented!()
            }
            async fn list_routes(
                &self,
                _: &SecurityContext,
                _: Option<Uuid>,
                _: &ListQuery,
            ) -> Result<Vec<Route>, DomainError> {
                unimplemented!()
            }
            async fn update_route(
                &self,
                _: &SecurityContext,
                _: Uuid,
                _: UpdateRouteRequest,
            ) -> Result<Route, DomainError> {
                unimplemented!()
            }
            async fn delete_route(&self, _: &SecurityContext, _: Uuid) -> Result<(), DomainError> {
                unimplemented!()
            }
            async fn resolve_proxy_target_for_client(
                &self,
                _: &SecurityContext,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<(Upstream, Route), DomainError> {
                unimplemented!()
            }
            async fn resolve_proxy_target(
                &self,
                grant: &ProxyGrant,
                _: &str,
                _: &str,
                _: &str,
                _: Tags,
            ) -> Result<ProxyTarget, DomainError> {
                let (upstream, route) = self
                    .0
                    .clone()
                    .ok_or_else(|| DomainError::not_found("upstream", Uuid::nil()))?;
                Ok(ProxyTarget {
                    auth_owner_tenant_id: upstream.tenant_id,
                    tenant_chain: vec![grant.ctx().subject_tenant_id()],
                    upstream,
                    route,
                    shadowed_route_limits: Vec::new(),
                })
            }
        }

        let cp: Arc<dyn ControlPlaneService> = Arc::new(NoopCp(resolved));
        crate::domain::test_support::ensure_crypto_provider();
        let engine = Arc::new(
            crate::infra::proxy::engine::ProxyEngine::new(
                &crate::infra::proxy::engine::EngineSettings {
                    connect_timeout: Duration::from_secs(10),
                    read_timeout: Duration::from_secs(30),
                    protocol_cache_ttl: Duration::from_secs(3600),
                    #[cfg(feature = "test-utils")]
                    skip_upstream_tls_verify: false,
                },
                Arc::new(crate::domain::ssrf::SsrfGuard::disabled()),
            )
            .expect("test proxy engine"),
        );

        DataPlaneServiceImpl::new(
            cp,
            credstore,
            policy_enforcer,
            None,
            TokenCacheConfig::default(),
            selector,
            engine,
            metrics,
            Arc::new(RateLimiter::new()),
            Arc::new(CircuitBreakers::new()),
        )
    }

    // P2 #12: Alias extraction happens on raw path, then suffix is normalized.
    // Path traversal in the alias segment must not influence which upstream is resolved.
    #[test]
    fn alias_extraction_ignores_path_traversal() {
        // Simulate what proxy_request does: extract alias from raw path, normalize suffix.
        fn extract(path: &str) -> (String, String) {
            let trimmed = path.strip_prefix('/').unwrap_or(path);
            let (alias, raw_suffix) = match trimmed.find('/') {
                Some(pos) => (&trimmed[..pos], &trimmed[pos..]),
                None => (trimmed, ""),
            };
            (alias.to_string(), normalize_path(raw_suffix))
        }

        // Normal case.
        let (alias, suffix) = extract("/myalias/v1/chat");
        assert_eq!(alias, "myalias");
        assert_eq!(suffix, "/v1/chat");

        // Path traversal attempt: alias is still the first raw segment.
        let (alias, suffix) = extract("/myalias/../admin/secret");
        assert_eq!(alias, "myalias");
        assert_eq!(suffix, "/admin/secret"); // ".." collapsed in suffix only

        // Deep traversal: alias is still literal first segment.
        let (alias, suffix) = extract("/myalias/../../etc/passwd");
        assert_eq!(alias, "myalias");
        assert_eq!(suffix, "/etc/passwd"); // ".." collapsed, clamped at root
    }

    // P2: HTTPS-only — Http scheme endpoint must be rejected.
    #[tokio::test]
    async fn select_endpoint_rejects_http_scheme() {
        let selector = Arc::new(MockSelector::new());
        let svc = build_svc(selector);

        // Single Http endpoint.
        let upstream = upstream_with(vec![Endpoint {
            scheme: Scheme::Http,
            host: "insecure.example.com".to_string(),
            port: 80,
        }]);
        let headers = HeaderMap::new();

        let err = svc.select_endpoint(&upstream, &headers, "/test").await;

        // select_endpoint itself doesn't enforce HTTPS — the check is in proxy_request
        // after select_endpoint returns. Verify the endpoint is returned here (enforcement
        // is at a higher level).
        assert!(err.is_ok(), "select_endpoint should return the endpoint");
        assert_eq!(err.unwrap().endpoint.scheme, Scheme::Http);
    }

    // positive-2.2 (custom-header-routing): X-OAGW-Target-Host matches an endpoint.
    #[tokio::test]
    async fn select_endpoint_target_host_matches() {
        let selector = Arc::new(MockSelector::new());
        let svc = build_svc(selector.clone());
        let upstream = upstream_with(vec![ep("a.com", 443), ep("b.com", 443)]);

        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-target-host", "a.com".parse().unwrap());

        let result = svc
            .select_endpoint(&upstream, &headers, "/test")
            .await
            .unwrap();
        assert_eq!(result.endpoint.host, "a.com");
        assert_eq!(selector.calls(), 0, "BackendSelector should not be called");
    }

    // negative-2.1 (custom-header-routing): X-OAGW-Target-Host does not match any endpoint.
    #[tokio::test]
    async fn select_endpoint_target_host_unknown() {
        let svc = build_svc(Arc::new(MockSelector::new()));
        let upstream = upstream_with(vec![ep("a.com", 443), ep("b.com", 443)]);

        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-target-host", "evil.com".parse().unwrap());

        let err = svc
            .select_endpoint(&upstream, &headers, "/test")
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::UnknownTargetHost { .. }),
            "expected UnknownTargetHost, got: {err:?}"
        );
    }

    // negative-1.2..1.4 (custom-header-routing): X-OAGW-Target-Host with invalid format.
    #[tokio::test]
    async fn select_endpoint_target_host_invalid_format() {
        let svc = build_svc(Arc::new(MockSelector::new()));
        let upstream = upstream_with(vec![ep("a.com", 443)]);

        for bad_value in [
            "a.com:443",
            "a.com/path",
            "a.com?q=1",
            "a b",
            "evil.com@real.com",
            "evil.com\\real.com",
            "a.com#fragment",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert("x-oagw-target-host", bad_value.parse().unwrap());
            let err = svc
                .select_endpoint(&upstream, &headers, "/test")
                .await
                .unwrap_err();
            assert!(
                matches!(err, DomainError::InvalidTargetHost { .. }),
                "expected InvalidTargetHost for '{bad_value}', got: {err:?}"
            );
        }

        // Empty header value: test separately since HeaderValue::from_static
        // allows empty strings while .parse() does not.
        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-target-host", HeaderValue::from_static(""));
        let err = svc
            .select_endpoint(&upstream, &headers, "/test")
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::InvalidTargetHost { .. }),
            "expected InvalidTargetHost for empty header, got: {err:?}"
        );
    }

    // positive-2.1 (custom-header-routing): Round-robin fallback for multi-endpoint (no header).
    #[tokio::test]
    async fn select_endpoint_round_robin_fallback() {
        let selector = Arc::new(MockSelector::new());
        let svc = build_svc(selector.clone());
        let upstream = upstream_with(vec![ep("a.com", 443), ep("b.com", 443)]);
        let headers = HeaderMap::new();

        let ep1 = svc
            .select_endpoint(&upstream, &headers, "/test")
            .await
            .unwrap();
        let ep2 = svc
            .select_endpoint(&upstream, &headers, "/test")
            .await
            .unwrap();

        assert_eq!(
            selector.calls(),
            2,
            "BackendSelector should be called for multi-endpoint"
        );
        // MockSelector returns endpoints in order: [0], [1], [0], ...
        assert_eq!(ep1.endpoint.host, "a.com");
        assert_eq!(ep2.endpoint.host, "b.com");
    }

    // positive-1.1 (custom-header-routing): Single-endpoint bypass (no header, no BackendSelector call).
    #[tokio::test]
    async fn select_endpoint_single_endpoint_bypass() {
        let selector = Arc::new(MockSelector::new());
        let svc = build_svc(selector.clone());
        let upstream = upstream_with(vec![ep("only.com", 443)]);
        let headers = HeaderMap::new();

        let result = svc
            .select_endpoint(&upstream, &headers, "/test")
            .await
            .unwrap();
        assert_eq!(result.endpoint.host, "only.com");
        assert_eq!(
            selector.calls(),
            0,
            "BackendSelector should NOT be called for single endpoint"
        );
    }

    // positive-1.2 (custom-header-routing): Single-endpoint upstream validates header if present.
    #[tokio::test]
    async fn select_endpoint_single_endpoint_validates_header() {
        let svc = build_svc(Arc::new(MockSelector::new()));
        let upstream = upstream_with(vec![ep("a.com", 443)]);

        // Valid header matching the single endpoint → OK.
        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-target-host", "a.com".parse().unwrap());
        let result = svc
            .select_endpoint(&upstream, &headers, "/test")
            .await
            .unwrap();
        assert_eq!(result.endpoint.host, "a.com");

        // Invalid header not matching → UnknownTargetHost.
        let mut headers = HeaderMap::new();
        headers.insert("x-oagw-target-host", "b.com".parse().unwrap());
        let err = svc
            .select_endpoint(&upstream, &headers, "/test")
            .await
            .unwrap_err();
        assert!(
            matches!(err, DomainError::UnknownTargetHost { .. }),
            "expected UnknownTargetHost for mismatched header on single-endpoint upstream"
        );
    }

    // -- Idle rate-limit bucket sweep --

    fn fast_refill_config() -> crate::domain::model::RateLimitConfig {
        use crate::domain::model::{
            RateLimitAlgorithm, RateLimitConfig, RateLimitScope, RateLimitStrategy, SustainedRate,
            Window,
        };
        // A spent token is back within a millisecond.
        RateLimitConfig {
            sharing: Default::default(),
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate {
                rate: 1_000,
                window: Window::Second,
            },
            burst: None,
            budget: None,
            scope: RateLimitScope::Global,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: false,
            shared_pool: None,
            limit_owner_id: None,
        }
    }

    #[tokio::test]
    async fn sweep_evicts_idle_buckets() {
        let limiter = Arc::new(RateLimiter::new());
        limiter
            .try_consume("key", &fast_refill_config(), "/test")
            .unwrap();
        let sweep = tokio::spawn(sweep_idle_state(
            Arc::downgrade(&limiter),
            Weak::new(),
            Arc::new(crate::domain::ports::NoopMetrics),
            Duration::from_millis(5),
            CancellationToken::new(),
        ));

        tokio::time::timeout(Duration::from_secs(5), async {
            while limiter.bucket_count() > 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the sweep evicts the refilled bucket");
        sweep.abort();
    }

    // -- check_rate_limits --

    fn limit(
        rate: u32,
        window: crate::domain::model::Window,
        scope: crate::domain::model::RateLimitScope,
    ) -> RateLimitConfig {
        use crate::domain::model::{RateLimitAlgorithm, RateLimitStrategy, SustainedRate};
        RateLimitConfig {
            sharing: Default::default(),
            algorithm: RateLimitAlgorithm::TokenBucket,
            sustained: SustainedRate { rate, window },
            burst: None,
            budget: None,
            scope,
            strategy: RateLimitStrategy::Reject,
            cost: 1,
            response_headers: true,
            shared_pool: None,
            limit_owner_id: None,
        }
    }

    fn route_on(
        upstream: &Upstream,
        rate_limit: Option<RateLimitConfig>,
    ) -> crate::domain::model::Route {
        use crate::domain::model::{MatchRules, Route};
        Route {
            id: Uuid::new_v4(),
            tenant_id: upstream.tenant_id,
            upstream_id: upstream.id,
            match_rules: MatchRules {
                http: None,
                grpc: None,
            },
            plugins: None,
            rate_limit,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
            managed_by: ManagedBy::Api,
        }
    }

    /// Whether each request passes, in order.
    fn admitted<'a>(
        limiter: &RateLimiter,
        upstream: &Upstream,
        requests: impl IntoIterator<
            Item = (
                &'a crate::domain::model::Route,
                Option<std::net::IpAddr>,
                &'a SecurityContext,
            ),
        >,
    ) -> Vec<bool> {
        requests
            .into_iter()
            .map(
                |(route, ip, ctx)| match check(limiter, upstream, route, &[], ip, ctx) {
                    Ok(_) => true,
                    Err(DomainError::RateLimitExceeded { .. }) => false,
                    Err(other) => panic!("unexpected error: {other:?}"),
                },
            )
            .collect()
    }

    fn check(
        limiter: &RateLimiter,
        upstream: &Upstream,
        route: &crate::domain::model::Route,
        shadowed: &[ShadowedRouteLimit],
        ip: Option<std::net::IpAddr>,
        ctx: &SecurityContext,
    ) -> Result<Option<(RateLimitOutcome, bool)>, DomainError> {
        check_rate_limits(
            limiter,
            upstream,
            route,
            shadowed,
            ip,
            ctx,
            "/t",
            &crate::domain::ports::NoopMetrics,
            "h",
            "/p",
        )
    }

    /// The address comes from the gateway's `ClientIp` extension only:
    /// forwarding headers the client sent are not read.
    #[test]
    fn the_client_ip_comes_from_the_gateway_extension() {
        let mut req = http::Request::builder()
            .uri("/alias/v1")
            .header("x-forwarded-for", "203.0.113.7")
            .header("x-real-ip", "198.51.100.9")
            .body(Body::Empty)
            .unwrap();
        let parsed = parse_proxy_request(req, 1024).unwrap();
        assert_eq!(parsed.client_ip, None);

        req = http::Request::builder()
            .uri("/alias/v1")
            .header("x-forwarded-for", "203.0.113.7")
            .body(Body::Empty)
            .unwrap();
        let ip: std::net::IpAddr = "192.0.2.10".parse().unwrap();
        req.extensions_mut().insert(toolkit::http::ClientIp(ip));
        assert_eq!(parse_proxy_request(req, 1024).unwrap().client_ip, Some(ip));
    }

    #[test]
    fn ip_scope_keys_on_the_resolved_address() {
        use crate::domain::model::{RateLimitScope, Window};
        let mut upstream = upstream_with(vec![]);
        upstream.rate_limit = Some(limit(1, Window::Minute, RateLimitScope::Ip));
        let route = route_on(&upstream, None);
        let ctx = test_ctx();
        let (a, b) = ("192.0.2.1".parse().ok(), "192.0.2.2".parse().ok());
        let limiter = RateLimiter::new();
        let requests = [a, a, b, None, None].map(|ip| (&route, ip, &ctx));
        assert_eq!(
            admitted(&limiter, &upstream, requests),
            [true, false, true, true, false]
        );
    }

    /// `scope: route` on an upstream limit gives every route its own bucket.
    #[test]
    fn upstream_scope_route_is_per_route() {
        use crate::domain::model::{RateLimitScope, Window};
        let mut upstream = upstream_with(vec![]);
        upstream.rate_limit = Some(limit(1, Window::Minute, RateLimitScope::Route));
        let (first, second) = (route_on(&upstream, None), route_on(&upstream, None));
        let ctx = test_ctx();
        let limiter = RateLimiter::new();
        let requests = [&first, &second, &first, &second].map(|r| (r, None, &ctx));
        assert_eq!(
            admitted(&limiter, &upstream, requests),
            [true, true, false, false]
        );
    }

    /// Route limits are checked on the route buckets; the upstream bucket
    /// keeps its own limit whichever route is called, so alternating routes
    /// does not refill it.
    #[test]
    fn alternating_routes_do_not_refill_the_upstream_bucket() {
        use crate::domain::model::{RateLimitScope, Window};
        let mut upstream = upstream_with(vec![]);
        upstream.rate_limit = Some(limit(2, Window::Minute, RateLimitScope::Tenant));
        let first = route_on(
            &upstream,
            Some(limit(10, Window::Hour, RateLimitScope::Tenant)),
        );
        let second = route_on(
            &upstream,
            Some(limit(20, Window::Hour, RateLimitScope::Tenant)),
        );
        let ctx = test_ctx();
        let limiter = RateLimiter::new();
        let requests = [&first, &second, &first, &second].map(|r| (r, None, &ctx));
        assert_eq!(
            admitted(&limiter, &upstream, requests),
            [true, true, false, false]
        );
    }

    /// A child's route that shadows the parent's route is limited by the
    /// parent route's enforced limit on the parent route's own bucket, the
    /// one a request matching the parent route uses. The upstream bucket
    /// keeps the upstream's limit.
    #[test]
    fn a_shadowed_enforced_route_limit_uses_its_own_bucket() {
        use crate::domain::model::{RateLimitScope, SharingMode, Window};
        let mut upstream = upstream_with(vec![]);
        upstream.rate_limit = Some(limit(100, Window::Minute, RateLimitScope::Tenant));
        let child_route = route_on(
            &upstream,
            Some(limit(50, Window::Minute, RateLimitScope::Tenant)),
        );
        let other_route = route_on(&upstream, None);
        let mut enforced = limit(2, Window::Minute, RateLimitScope::Tenant);
        enforced.sharing = SharingMode::Enforce;
        let parent_upstream = upstream_with(vec![]);
        let parent_route = route_on(&parent_upstream, Some(enforced.clone()));
        let shadowed = [ShadowedRouteLimit {
            route_id: parent_route.id,
            rate_limit: enforced,
        }];
        let ctx = test_ctx();
        let limiter = RateLimiter::new();

        for remaining in [1, 0] {
            let (outcome, shown) = check(&limiter, &upstream, &child_route, &shadowed, None, &ctx)
                .unwrap()
                .unwrap();
            assert_eq!(
                (outcome.limit, outcome.remaining, shown),
                (2, remaining, true)
            );
        }
        match check(&limiter, &upstream, &child_route, &shadowed, None, &ctx) {
            Err(DomainError::RateLimitExceeded {
                limit,
                remaining,
                retry_after_secs,
                ..
            }) => {
                assert_eq!((limit, remaining), (Some(2), Some(0)));
                assert!(retry_after_secs.is_some_and(|s| s > 0));
            }
            other => panic!("expected RateLimitExceeded, got {other:?}"),
        }

        // The parent route's bucket is the one its own requests use.
        assert!(matches!(
            check(&limiter, &upstream, &parent_route, &[], None, &ctx),
            Err(DomainError::RateLimitExceeded { .. })
        ));

        // The upstream bucket kept its limit of 100, drawn on only by the
        // three admitted requests: route buckets are checked first.
        let (outcome, _) = check(&limiter, &upstream, &other_route, &[], None, &ctx)
            .unwrap()
            .unwrap();
        assert_eq!((outcome.limit, outcome.remaining), (100, 97));
    }

    /// A request a route limit rejects is not charged to the upstream bucket
    /// or the shared pool: a tenant calling a route it has used up leaves the
    /// pool to its siblings.
    #[test]
    fn a_route_rejection_does_not_charge_the_upstream_or_the_pool() {
        use crate::domain::model::{RateLimitAlgorithm, RateLimitScope, SharedPool, Window};
        let mut upstream = upstream_with(vec![]);
        let mut rl = limit(3, Window::Minute, RateLimitScope::Tenant);
        rl.shared_pool = Some(SharedPool {
            owner_id: Uuid::new_v4(),
            total: 3,
            window: Window::Minute,
            algorithm: RateLimitAlgorithm::TokenBucket,
        });
        upstream.rate_limit = Some(rl);
        let used_up = route_on(
            &upstream,
            Some(limit(1, Window::Minute, RateLimitScope::Tenant)),
        );
        let open = route_on(&upstream, None);
        let (a, b) = (test_ctx(), test_ctx());
        let limiter = RateLimiter::new();
        let requests = [
            (&used_up, None, &a),
            (&used_up, None, &a),
            (&used_up, None, &a),
            (&open, None, &a),
            (&open, None, &b),
            (&open, None, &b),
        ];
        assert_eq!(
            admitted(&limiter, &upstream, requests),
            [true, false, false, true, true, false]
        );
    }

    /// A shared budget adds one pool bucket of `total` per window for every
    /// tenant drawing on it, next to each tenant's own bucket.
    #[test]
    fn a_shared_pool_is_drawn_on_by_every_tenant() {
        use crate::domain::model::{RateLimitAlgorithm, RateLimitScope, SharedPool, Window};
        let mut upstream = upstream_with(vec![]);
        let mut rl = limit(100, Window::Minute, RateLimitScope::Tenant);
        rl.shared_pool = Some(SharedPool {
            owner_id: Uuid::new_v4(),
            total: 3,
            window: Window::Minute,
            algorithm: RateLimitAlgorithm::TokenBucket,
        });
        upstream.rate_limit = Some(rl);
        let route = route_on(&upstream, None);
        let (a, b) = (test_ctx(), test_ctx());
        let limiter = RateLimiter::new();
        let requests = [&a, &b, &a, &b].map(|ctx| (&route, None, ctx));
        assert_eq!(
            admitted(&limiter, &upstream, requests),
            [true, true, true, false]
        );

        // Without the pool each tenant has its own 100.
        let limiter = RateLimiter::new();
        upstream.rate_limit.as_mut().unwrap().shared_pool = None;
        let requests = [&a, &b, &a, &b].map(|ctx| (&route, None, ctx));
        assert_eq!(admitted(&limiter, &upstream, requests), [true; 4]);
    }

    /// A tenant held to another upstream's limit on this upstream uses a
    /// bucket of its own (`via:{owner}`), not the one the upstream's own limit
    /// fills. Under a shared pool the scoped bucket is keyed on the pool
    /// owner: tenants that inherit the owner's limit share it, a tenant whose
    /// own limit applies gets its own.
    #[test]
    fn a_borrowed_limit_does_not_use_the_owners_bucket() {
        use crate::domain::model::{RateLimitAlgorithm, RateLimitScope, SharedPool, Window};
        let mut own = upstream_with(vec![]);
        let mut rl = limit(1, Window::Minute, RateLimitScope::Global);
        rl.limit_owner_id = Some(own.id);
        own.rate_limit = Some(rl);
        let mut borrowed = own.clone();
        let mut rl = limit(100, Window::Minute, RateLimitScope::Global);
        rl.limit_owner_id = Some(Uuid::new_v4());
        borrowed.rate_limit = Some(rl);
        let route = route_on(&own, None);
        let ctx = test_ctx();
        let limiter = RateLimiter::new();
        // In one bucket the borrowed request would use up the owner's 1/min.
        let calls = [&borrowed, &own, &own, &borrowed]
            .map(|u| check(&limiter, u, &route, &[], None, &ctx).is_ok());
        assert_eq!(calls, [true, true, false, true]);

        let pool_owner = Uuid::new_v4();
        let pool = Some(SharedPool {
            owner_id: pool_owner,
            total: 100,
            window: Window::Minute,
            algorithm: RateLimitAlgorithm::TokenBucket,
        });
        // Two tenants inheriting the pool owner's 1/min share its bucket.
        let mut first = own.clone();
        let mut second = own.clone();
        second.id = Uuid::new_v4();
        // A tenant whose own 1/min applies keys `via` itself.
        let mut private = own.clone();
        for (u, owner) in [
            (&mut first, pool_owner),
            (&mut second, pool_owner),
            (&mut private, Uuid::new_v4()),
        ] {
            let rl = u.rate_limit.as_mut().unwrap();
            rl.sustained.rate = 1;
            rl.shared_pool = pool;
            rl.limit_owner_id = Some(owner);
        }
        let limiter = RateLimiter::new();
        let calls = [&first, &second, &private]
            .map(|u| check(&limiter, u, &route, &[], None, &ctx).is_ok());
        assert_eq!(calls, [true, false, true]);
    }

    /// Keeps only the request-duration observations.
    #[derive(Default)]
    struct DurationRecorder(std::sync::Mutex<Vec<(String, String, String)>>);

    impl DurationRecorder {
        fn phases(&self, phase: &str) -> Vec<(String, String)> {
            self.0
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, _, p)| p == phase)
                .map(|(host, path, _)| (host.clone(), path.clone()))
                .collect()
        }
    }

    impl OagwMetricsPort for DurationRecorder {
        fn record_request(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: u16,
            _: Option<crate::domain::plugin::credentials::CredentialMode>,
        ) {
        }
        fn record_error(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<crate::domain::plugin::credentials::CredentialMode>,
        ) {
        }
        fn record_request_duration_seconds(&self, host: &str, path: &str, phase: &str, _: f64) {
            self.0
                .lock()
                .unwrap()
                .push((host.to_owned(), path.to_owned(), phase.to_owned()));
        }
        fn increment_in_flight(&self, _: &str) {}
        fn decrement_in_flight(&self, _: &str) {}
        fn record_rate_limit_exceeded(
            &self,
            _: &str,
            _: &str,
            _: crate::domain::ports::metric_labels::RateLimitBucket,
        ) {
        }
        fn record_rate_limit_usage_ratio(
            &self,
            _: &str,
            _: &str,
            _: crate::domain::ports::metric_labels::RateLimitBucket,
            _: f64,
        ) {
        }
        fn increment_active_websocket_sessions(&self, _: &str) {}
        fn decrement_active_websocket_sessions(&self, _: &str) {}
        fn record_websocket_session_duration_seconds(
            &self,
            _: &str,
            _: crate::domain::ports::metric_labels::StreamOutcome,
            _: f64,
        ) {
        }
        fn record_stream_duration_seconds(
            &self,
            _: &str,
            _: &str,
            _: crate::domain::ports::metric_labels::StreamOutcome,
            _: f64,
        ) {
        }
        fn record_rate_limit_sweep(&self, _: u64, _: u64) {}
        fn record_registry_provisioning_failure(&self, _: &str, _: &str) {}
        fn record_registry_provisioning_run(&self, _: &str) {}
        fn record_credential_withheld(&self, _: &str, _: &'static str) {}
        fn record_proxy_cache_lookup(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: crate::domain::ports::metric_labels::CacheLookup,
        ) {
        }
        fn record_proxy_cache_entries(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: u64,
        ) {
        }
        fn record_proxy_cache_evictions(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: crate::domain::ports::metric_labels::CacheEviction,
            _: u64,
        ) {
        }
        fn record_proxy_cache_change(&self, _: &str) {}
        fn record_proxy_cache_flush(&self, _: &str) {}
        fn record_proxy_cache_load_discarded(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
        ) {
        }
        fn record_proxy_cache_stale_served(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: crate::domain::ports::metric_labels::CacheStale,
        ) {
        }
        fn record_proxy_cache_publish_failures(&self, _: &str, _: u64) {}
    }

    fn test_ctx() -> SecurityContext {
        SecurityContext::builder()
            .subject_tenant_id(Uuid::new_v4())
            .subject_id(Uuid::new_v4())
            .build()
            .expect("test security context")
    }

    fn proxy_get(uri: &str) -> http::Request<Body> {
        http::Request::builder()
            .method(http::Method::GET)
            .uri(uri)
            .body(Body::Empty)
            .unwrap()
    }

    /// A resolved request records the `resolve` phase once, under the
    /// upstream's alias and the route's path.
    #[tokio::test]
    async fn resolve_phase_recorded_with_resolved_labels() {
        use crate::domain::model::{HttpMatch, HttpMethod, MatchRules, Route};

        let upstream = upstream_with(vec![Endpoint {
            scheme: Scheme::Https,
            host: "127.0.0.1".into(),
            port: 1,
        }]);
        let route = Route {
            id: Uuid::new_v4(),
            tenant_id: upstream.tenant_id,
            upstream_id: upstream.id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Get],
                    path: "/v1".into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
            managed_by: ManagedBy::Api,
        };
        let metrics = Arc::new(DurationRecorder::default());
        let svc = build_svc_resolving(
            Arc::new(MockSelector::new()),
            metrics.clone(),
            Some((upstream, route)),
        );

        // The query parameter is not allowlisted: the request fails right
        // after resolution, before any network call.
        let err = svc
            .proxy_request(test_ctx(), proxy_get("/test/v1/chat?x=1"))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");

        assert_eq!(
            metrics.phases(phase::RESOLVE),
            vec![("test".to_owned(), "/v1".to_owned())]
        );
    }

    /// A request that does not resolve still records the `resolve` phase,
    /// under the unknown labels.
    #[tokio::test]
    async fn resolve_phase_recorded_when_resolution_fails() {
        let metrics = Arc::new(DurationRecorder::default());
        let svc = build_svc_resolving(Arc::new(MockSelector::new()), metrics.clone(), None);

        let err = svc
            .proxy_request(test_ctx(), proxy_get("/missing/v1/chat"))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotFound { .. }), "{err:?}");

        let unknown = metric_labels::UNKNOWN.to_owned();
        assert_eq!(
            metrics.phases(phase::RESOLVE),
            vec![(unknown.clone(), unknown)]
        );
    }

    /// Keeps the request counters, durations and in-flight changes.
    #[derive(Default)]
    pub(crate) struct RequestRecorder(std::sync::Mutex<Vec<String>>);

    impl RequestRecorder {
        pub(crate) fn events(&self) -> Vec<String> {
            self.0.lock().unwrap().clone()
        }
        fn push(&self, event: String) {
            self.0.lock().unwrap().push(event);
        }
    }

    impl OagwMetricsPort for RequestRecorder {
        fn record_request(
            &self,
            host: &str,
            _: &str,
            method: &str,
            status: u16,
            _: Option<crate::domain::plugin::credentials::CredentialMode>,
        ) {
            self.push(format!("request {host} {method} {status}"));
        }
        fn record_error(
            &self,
            host: &str,
            _: &str,
            variant: &str,
            _: Option<crate::domain::plugin::credentials::CredentialMode>,
        ) {
            self.push(format!("error {host} {variant}"));
        }
        fn record_request_duration_seconds(&self, _: &str, _: &str, phase: &str, _: f64) {
            self.push(format!("duration {phase}"));
        }
        fn record_stream_duration_seconds(
            &self,
            host: &str,
            _: &str,
            outcome: crate::domain::ports::metric_labels::StreamOutcome,
            _: f64,
        ) {
            self.push(format!("stream {host} {}", outcome.as_str()));
        }
        fn increment_in_flight(&self, host: &str) {
            self.push(format!("in_flight +1 {host}"));
        }
        fn decrement_in_flight(&self, host: &str) {
            self.push(format!("in_flight -1 {host}"));
        }
        fn record_rate_limit_exceeded(&self, _: &str, _: &str, bucket: RateLimitBucket) {
            self.push(format!("exceeded {}", bucket.as_str()));
        }
        fn record_rate_limit_usage_ratio(
            &self,
            _: &str,
            _: &str,
            bucket: RateLimitBucket,
            ratio: f64,
        ) {
            self.push(format!("usage {} {ratio}", bucket.as_str()));
        }
        fn increment_active_websocket_sessions(&self, _: &str) {}
        fn decrement_active_websocket_sessions(&self, _: &str) {}
        fn record_websocket_session_duration_seconds(
            &self,
            _: &str,
            _: crate::domain::ports::metric_labels::StreamOutcome,
            _: f64,
        ) {
        }

        fn record_rate_limit_sweep(&self, _: u64, _: u64) {}
        fn record_registry_provisioning_failure(&self, _: &str, _: &str) {}
        fn record_registry_provisioning_run(&self, _: &str) {}
        fn record_credential_withheld(&self, _: &str, _: &'static str) {}
        fn record_proxy_cache_lookup(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: crate::domain::ports::metric_labels::CacheLookup,
        ) {
        }
        fn record_proxy_cache_entries(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: u64,
        ) {
        }
        fn record_proxy_cache_evictions(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: crate::domain::ports::metric_labels::CacheEviction,
            _: u64,
        ) {
        }
        fn record_proxy_cache_change(&self, _: &str) {}
        fn record_proxy_cache_flush(&self, _: &str) {}
        fn record_proxy_cache_load_discarded(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
        ) {
        }
        fn record_proxy_cache_stale_served(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: crate::domain::ports::metric_labels::CacheStale,
        ) {
        }
        fn record_proxy_cache_publish_failures(&self, _: &str, _: u64) {}
    }

    /// `authorize_proxy` counts a denial, with the request's method and the
    /// 403 the client gets; a grant held counts nothing, since the proxied
    /// request is counted when it completes (#4981, #4989).
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn authorize_proxy_counts_only_a_denial() {
        let metrics = Arc::new(RequestRecorder::default());
        let allowed =
            build_svc_with_decision(Arc::new(MockSelector::new()), metrics.clone(), None, true);
        let _grant = allowed
            .authorize_proxy(test_ctx(), &http::Method::POST)
            .await
            .unwrap();
        assert!(metrics.events().is_empty(), "{:?}", metrics.events());

        let denied =
            build_svc_with_decision(Arc::new(MockSelector::new()), metrics.clone(), None, false);
        let err = denied
            .authorize_proxy(test_ctx(), &http::Method::POST)
            .await
            .unwrap_err();
        assert!(logs_contain("OAGW proxy call denied"));
        assert!(matches!(err, DomainError::Forbidden { .. }), "{err:?}");
        assert_eq!(
            metrics.events(),
            [
                "duration total",
                "request unknown POST 403",
                "error unknown Forbidden",
            ]
        );
    }

    /// A plaintext upstream is refused before the auth plugin runs, so no
    /// credential is prepared for it (#4974).
    #[tokio::test]
    async fn http_upstream_is_refused_before_auth() {
        use crate::domain::model::{
            AuthConfig, HttpMatch, HttpMethod, MatchRules, Route, SharingMode,
        };
        use toolkit_gts::gts_id;
        let mut upstream = upstream_with(vec![Endpoint {
            scheme: Scheme::Http,
            host: "127.0.0.1".into(),
            port: 80,
        }]);
        // An auth plugin that would fail if it ran.
        upstream.auth = Some(AuthConfig {
            plugin_type: gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.missing.v1").into(),
            sharing: SharingMode::Private,
            config: None,
        });
        let route = Route {
            id: Uuid::new_v4(),
            tenant_id: upstream.tenant_id,
            upstream_id: upstream.id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Get],
                    path: "/v1".into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
            managed_by: ManagedBy::Api,
        };
        let svc = build_svc_resolving(
            Arc::new(MockSelector::new()),
            Arc::new(crate::domain::ports::NoopMetrics),
            Some((upstream, route)),
        );
        match svc.proxy_request(test_ctx(), proxy_get("/test/v1")).await {
            Err(DomainError::Validation {
                field: f,
                reason,
                instance,
                ..
            }) => {
                assert_eq!(f, "server.endpoints.scheme");
                assert_eq!(reason, field::HTTP_UPSTREAM_FORBIDDEN);
                assert!(instance.ends_with("/test/v1"), "{instance}");
            }
            other => panic!("expected HTTP_UPSTREAM_FORBIDDEN, got {other:?}"),
        }
    }

    /// An HTTPS upstream with apikey auth reading `secret_ref`, and a
    /// `GET /v1` route on it.
    fn apikey_target(secret_ref: &str) -> (Upstream, crate::domain::model::Route) {
        use crate::domain::model::{
            AuthConfig, HttpMatch, HttpMethod, MatchRules, Route, SharingMode,
        };
        let mut upstream = upstream_with(vec![Endpoint {
            scheme: Scheme::Https,
            host: "127.0.0.1".into(),
            port: 1,
        }]);
        upstream.auth = Some(AuthConfig {
            plugin_type: crate::domain::gts_helpers::APIKEY_AUTH_PLUGIN_ID.into(),
            sharing: SharingMode::Private,
            config: Some(
                [
                    ("header".to_owned(), "authorization".to_owned()),
                    ("prefix".to_owned(), "Bearer ".to_owned()),
                    ("secret_ref".to_owned(), secret_ref.to_owned()),
                ]
                .into_iter()
                .collect(),
            ),
        });
        let route = Route {
            id: Uuid::new_v4(),
            tenant_id: upstream.tenant_id,
            upstream_id: upstream.id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Get],
                    path: "/v1".into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
            managed_by: ManagedBy::Api,
        };
        (upstream, route)
    }

    /// A secret that is gone at proxy time, or a credential store failure,
    /// names the auth config key in the error and in the warn log of the auth
    /// plugin step. The reference reaches neither the error returned to the
    /// caller nor the logs (#4989). A missing secret's 401 carries no
    /// diagnostic, so the warn log is where the key shows.
    #[tokio::test]
    #[tracing_test::traced_test]
    async fn proxy_time_secret_failures_do_not_reveal_the_ref() {
        use crate::domain::test_support::ErroringCredStore;
        use credstore_sdk::{CredStoreClientV1, CredStoreError};
        use toolkit_canonical_errors::CanonicalError;

        let stores: [Arc<dyn CredStoreClientV1>; 3] = [
            Arc::new(credstore_sdk::test_util::MockCredStoreClient::empty()),
            Arc::new(ErroringCredStore(|| {
                CredStoreError::Internal("failed reading leaked-ref-77".into())
            })),
            Arc::new(ErroringCredStore(|| CredStoreError::ServiceUnavailable {
                detail: "leaked-ref-77: backend down".into(),
                retry_after: None,
            })),
        ];
        for (i, store) in stores.into_iter().enumerate() {
            let svc = build_svc_with_credstore(
                Arc::new(MockSelector::new()),
                Arc::new(crate::domain::ports::NoopMetrics),
                Some(apikey_target("cred://leaked-ref-77")),
                true,
                store,
            );
            let err = svc
                .proxy_request(test_ctx(), proxy_get("/test/v1"))
                .await
                .unwrap_err();
            let text = format!("{err:?}");
            assert!(text.contains("auth.config.secret_ref"), "{text}");
            assert!(!text.contains("leaked-ref-77"), "{text}");
            if i == 0 {
                assert!(matches!(err, DomainError::SecretNotFound { .. }), "{err:?}");
            } else {
                assert!(
                    matches!(err, DomainError::CredentialStoreUnavailable { .. }),
                    "{err:?}"
                );
            }

            let canonical = CanonicalError::from(err);
            let text = format!("{canonical:?}");
            assert!(!text.contains("leaked-ref-77"), "{text}");
            if i == 0 {
                assert!(
                    matches!(&canonical, CanonicalError::Unauthenticated { ctx, .. }
                        if ctx.reason.as_deref() == Some(oagw_sdk::reason::auth::CREDENTIAL_NOT_FOUND)),
                    "{canonical:?}"
                );
                // One warn, naming the key and the store's error kind.
                assert!(logs_contain("auth.config.secret_ref"));
                assert!(logs_contain("secret not found"));
                assert!(!logs_contain("leaked-ref-77"));
            }
            // One warn line per failed request, however often the error is
            // converted: so far one missing secret and `i` store outages.
            logs_assert(|lines: &[&str]| {
                assert_log_count(lines, "auth plugin: credential not accessible", 1)?;
                assert_log_count(lines, "auth plugin: credential store unavailable", i)
            });
        }
        assert!(logs_contain("executing auth plugin"));
        assert!(!logs_contain("leaked-ref-77"));
    }

    /// Fails unless exactly `expected` log lines contain `message`.
    fn assert_log_count(lines: &[&str], message: &str, expected: usize) -> Result<(), String> {
        let found: Vec<_> = lines.iter().filter(|l| l.contains(message)).collect();
        if found.len() == expected {
            Ok(())
        } else {
            Err(format!(
                "expected {expected} line(s) with {message:?}, got {found:#?}"
            ))
        }
    }

    /// An identity provider outage and an invalid IdP response are each
    /// warned once, by the auth plugin step with the request's identifiers;
    /// the REST conversions, which run more than once per error, only log at
    /// debug.
    #[test]
    #[tracing_test::traced_test]
    fn idp_failures_warn_once() {
        use toolkit_canonical_errors::CanonicalError;

        let scope = AuthScope {
            caller_tenant_id: uuid::Uuid::nil(),
            upstream_id: uuid::Uuid::nil(),
            upstream_alias: "idp-upstream",
            plugin_type: "oauth2-test",
        };
        let cases = [
            (
                PluginError::IdpUnavailable("token endpoint returned 503".into()),
                "auth plugin: OAuth2 identity provider unavailable",
                "token endpoint returned 503",
            ),
            (
                PluginError::IdpInvalidResponse("unsupported token type: mac".into()),
                "auth plugin: identity provider sent an invalid response",
                "unsupported token type: mac",
            ),
        ];
        let mut checks = Vec::new();
        for (plugin_err, message, detail) in cases {
            let err = auth_plugin_error(plugin_err, "/i", scope);
            for _ in 0..3 {
                let _ = CanonicalError::from(err.clone());
            }
            checks.push((message, detail));
        }

        logs_assert(|lines: &[&str]| {
            for (message, detail) in &checks {
                let warns: Vec<_> = lines
                    .iter()
                    .filter(|l| l.contains(" WARN ") && l.contains(detail))
                    .collect();
                match warns.as_slice() {
                    [line]
                        if line.contains(message)
                            && line.contains("upstream_alias=\"idp-upstream\"")
                            && line.contains("plugin=\"oauth2-test\"") => {}
                    other => {
                        return Err(format!(
                            "expected one warn {message:?} with the scope, got {other:#?}"
                        ));
                    }
                }
                assert_log_count(lines, message, 1)?;
            }
            Ok(())
        });
    }

    /// The auth plugin sets only its own header. Repeated client headers and
    /// non-UTF-8 values reach the upstream as sent, and the client's own
    /// line of the plugin's header is replaced, not kept next to it (#4982).
    #[tokio::test]
    async fn auth_plugin_keeps_repeated_and_non_utf8_headers() {
        let svc = build_svc_with_credstore(
            Arc::new(MockSelector::new()),
            Arc::new(crate::domain::ports::NoopMetrics),
            None,
            true,
            Arc::new(credstore_sdk::test_util::MockCredStoreClient::with_secrets(
                vec![("cred://k".into(), "sk-1".into())],
            )),
        );
        let (upstream, _) = apikey_target("cred://k");
        let auth = upstream.auth.clone().unwrap();

        let raw = HeaderValue::from_bytes(b"caf\xe9").unwrap();
        let mut headers = HeaderMap::new();
        headers.append("x-multi", HeaderValue::from_static("a"));
        headers.append("x-multi", HeaderValue::from_static("b"));
        headers.insert("x-raw", raw.clone());
        headers.append("authorization", HeaderValue::from_static("Bearer client-1"));
        headers.append("authorization", HeaderValue::from_static("Bearer client-2"));

        execute_auth_plugin(
            &svc.auth_registry,
            &upstream,
            &auth,
            &mut headers,
            &test_ctx(),
            "/i",
        )
        .await
        .unwrap();

        let multi: Vec<_> = headers.get_all("x-multi").iter().collect();
        assert_eq!(multi, ["a", "b"]);
        assert_eq!(headers.get("x-raw"), Some(&raw));
        let auth_lines: Vec<_> = headers.get_all("authorization").iter().collect();
        assert_eq!(auth_lines, ["Bearer sk-1"]);
        assert!(auth_lines[0].is_sensitive());
    }

    /// A scope for calling `auth_plugin_error` directly.
    fn test_auth_scope() -> AuthScope<'static> {
        AuthScope {
            caller_tenant_id: uuid::Uuid::nil(),
            upstream_id: uuid::Uuid::nil(),
            upstream_alias: "test",
            plugin_type: "test",
        }
    }

    /// A header the plugin cannot set fails the request instead of sending
    /// it without credentials; the error names the header, not the value.
    #[tokio::test]
    async fn auth_plugin_invalid_header_fails_the_request() {
        let mut headers = HeaderMap::new();
        let err =
            set_auth_headers(&mut headers, vec![("bad header".into(), "sk-1".into())]).unwrap_err();
        assert!(matches!(err, PluginError::InvalidConfig(_)), "{err:?}");

        let err = set_auth_headers(
            &mut headers,
            vec![("authorization".into(), "Bearer sk-1\r\nx: y".into())],
        )
        .unwrap_err();
        let PluginError::InvalidConfig(msg) = &err else {
            panic!("expected InvalidConfig, got {err:?}");
        };
        assert!(msg.contains("authorization"), "{msg}");
        assert!(!msg.contains("sk-1"), "{msg}");
        assert!(headers.is_empty(), "{headers:?}");

        match auth_plugin_error(err, "/i", test_auth_scope()) {
            DomainError::Internal { message } => {
                assert!(message.contains("authorization"), "{message}");
                assert!(!message.contains("sk-1"), "{message}");
            }
            other => panic!("expected a misconfiguration (internal), got {other:?}"),
        }
    }

    /// Every auth plugin error goes through one mapping, whichever plugin
    /// raised it: the status and reason the REST layer sends for each.
    #[test]
    fn auth_plugin_errors_have_one_mapping() {
        use oagw_sdk::reason::{auth, unavailable};
        use std::time::Duration;
        use toolkit_canonical_errors::{CanonicalError, Problem};

        let cases: Vec<(PluginError, u16, Option<&str>)> = vec![
            (
                PluginError::SecretNotFound {
                    key: "secret_ref",
                    kind: "secret not found",
                },
                401,
                Some(auth::CREDENTIAL_NOT_FOUND),
            ),
            (
                PluginError::CredentialStoreUnavailable {
                    detail: "credential store unavailable for auth.config.secret_ref".into(),
                    retry_after: Some(Duration::from_secs(9)),
                },
                503,
                Some(unavailable::CREDENTIAL_STORE_UNAVAILABLE),
            ),
            (
                PluginError::IdpUnavailable("token endpoint returned 503".into()),
                503,
                Some(unavailable::IDENTITY_PROVIDER_UNAVAILABLE),
            ),
            (
                PluginError::IdpInvalidResponse("unsupported token type: mac".into()),
                502,
                Some(unavailable::REQUEST_NOT_SENT),
            ),
            (
                PluginError::AuthFailed("identity provider rejected the client".into()),
                401,
                Some(auth::PLUGIN_FAILED),
            ),
            (
                PluginError::Rejected("refused".into()),
                401,
                Some(auth::PLUGIN_FAILED),
            ),
            (
                PluginError::InvalidConfig("missing client_id_ref".into()),
                500,
                None,
            ),
            (
                PluginError::InvalidRequest {
                    field: "body",
                    detail: "a streamed request body cannot be hashed".into(),
                },
                400,
                None,
            ),
            (
                PluginError::Internal("secret value is not valid UTF-8".into()),
                401,
                Some(auth::PLUGIN_INTERNAL),
            ),
        ];
        for (err, status, reason) in cases {
            let label = format!("{err:?}");
            let problem = Problem::from(CanonicalError::from(auth_plugin_error(
                err,
                "/i",
                test_auth_scope(),
            )));
            assert_eq!(problem.status, Some(status), "{label}");
            assert_eq!(
                problem.context.get("reason").and_then(|r| r.as_str()),
                reason,
                "{label}"
            );
        }
    }

    /// A request transform running after the auth plugin applies only its
    /// own change: the credential keeps its bytes (obs-text here, which the
    /// transform's string view cannot hold) and its sensitive flag, and
    /// repeated and non-UTF-8 client lines survive.
    #[tokio::test]
    async fn transform_after_auth_keeps_credential_and_client_headers() {
        let svc = build_svc_with_credstore(
            Arc::new(MockSelector::new()),
            Arc::new(crate::domain::ports::NoopMetrics),
            None,
            true,
            Arc::new(credstore_sdk::test_util::MockCredStoreClient::with_secrets(
                vec![("cred://k".into(), "sk-café".into())],
            )),
        );
        let (upstream, _) = apikey_target("cred://k");
        let auth = upstream.auth.clone().unwrap();

        let raw = HeaderValue::from_bytes(b"caf\xe9").unwrap();
        let mut headers = HeaderMap::new();
        headers.append("x-multi", HeaderValue::from_static("a"));
        headers.append("x-multi", HeaderValue::from_static("b"));
        headers.insert("x-raw", raw.clone());
        execute_auth_plugin(
            &svc.auth_registry,
            &upstream,
            &auth,
            &mut headers,
            &test_ctx(),
            "/i",
        )
        .await
        .unwrap();

        let request_id = crate::domain::model::PluginBinding {
            plugin_ref: crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID.into(),
            config: Default::default(),
        };
        let mut query = Vec::new();
        execute_transform_requests(
            &svc.transform_registry,
            &[&request_id],
            &mut headers,
            &mut query,
            "GET",
            "/",
            &test_ctx(),
        )
        .await;

        let auth_lines: Vec<_> = headers.get_all("authorization").iter().collect();
        assert_eq!(auth_lines.len(), 1, "{headers:?}");
        assert_eq!(auth_lines[0].as_bytes(), "Bearer sk-café".as_bytes());
        assert!(auth_lines[0].is_sensitive());
        let multi: Vec<_> = headers.get_all("x-multi").iter().collect();
        assert_eq!(multi, ["a", "b"]);
        assert_eq!(headers.get("x-raw"), Some(&raw));
        assert_eq!(headers.get_all("x-request-id").iter().count(), 1);
    }

    /// A response transform's set, append and remove apply; names it does
    /// not touch keep their lines, bytes and sensitive flag.
    #[tokio::test]
    async fn response_transform_applies_only_its_changes() {
        use crate::domain::plugin::{TransformPlugin, TransformResponseContext};

        struct EditHeaders;
        #[async_trait]
        impl TransformPlugin for EditHeaders {
            async fn on_response(
                &self,
                ctx: &mut TransformResponseContext,
            ) -> Result<(), PluginError> {
                ctx.headers.retain(|(k, _)| k != "x-gone" && k != "x-set");
                ctx.headers.push(("X-Set".into(), "new".into()));
                ctx.headers.push(("x-added".into(), "1".into()));
                ctx.headers.push(("x-added".into(), "2".into()));
                Ok(())
            }
        }
        let plugin_ref =
            toolkit_gts::gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.edit_test.v1");
        let mut registry = TransformPluginRegistry::with_builtins();
        registry.insert(plugin_ref, Arc::new(EditHeaders));
        let binding = crate::domain::model::PluginBinding {
            plugin_ref: plugin_ref.into(),
            config: Default::default(),
        };

        let raw = HeaderValue::from_bytes(b"caf\xe9").unwrap();
        let mut secret = HeaderValue::from_static("token");
        secret.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.append("x-multi", HeaderValue::from_static("a"));
        headers.append("x-multi", HeaderValue::from_static("b"));
        headers.insert("x-raw", raw.clone());
        headers.insert("x-secret", secret);
        headers.insert("x-gone", HeaderValue::from_static("1"));
        headers.append("x-set", HeaderValue::from_static("old"));
        headers.append("x-set", raw.clone());

        execute_transform_responses(
            &registry,
            &[&binding],
            http::StatusCode::OK,
            &mut headers,
            &test_ctx(),
        )
        .await;

        let multi: Vec<_> = headers.get_all("x-multi").iter().collect();
        assert_eq!(multi, ["a", "b"]);
        assert_eq!(headers.get("x-raw"), Some(&raw));
        assert!(headers["x-secret"].is_sensitive());
        assert!(!headers.contains_key("x-gone"));
        // A changed name is replaced whole, its unseen non-UTF-8 line too.
        let set: Vec<_> = headers.get_all("x-set").iter().collect();
        assert_eq!(set, ["new"]);
        let added: Vec<_> = headers.get_all("x-added").iter().collect();
        assert_eq!(added, ["1", "2"]);
    }

    /// The handle the gear stops streams with is the one this service
    /// subscribes its SSE bodies and WebSocket relays to.
    #[tokio::test]
    async fn the_stream_shutdown_handle_is_the_one_streams_watch() {
        let svc = build_svc_resolving(
            Arc::new(MockSelector::new()),
            Arc::new(crate::domain::ports::NoopMetrics),
            None,
        );
        let watched = svc.stream_shutdown.subscribe();
        svc.stream_shutdown().end_streams();
        tokio::time::timeout(Duration::from_secs(1), watched.stopped())
            .await
            .expect("a stop through the handed-out handle reaches the streams");
    }

    /// Rows stored before plugin ids were checked on write (#4974) may still
    /// name a plugin that does not resolve. Pins what the proxy does with
    /// them today: an auth plugin fails the request with
    /// `AUTH_PLUGIN_NOT_FOUND`, a guard with an internal error, and a
    /// transform is skipped, as is a short id with no schema prefix. #4985
    /// owns the target behavior (scenario 4.5-B).
    #[tokio::test]
    async fn unresolvable_plugins_on_stored_rows() {
        use crate::domain::model::{AuthConfig, PluginBinding, SharingMode};
        use toolkit_gts::gts_id;
        let svc = build_svc_resolving(
            Arc::new(MockSelector::new()),
            Arc::new(crate::domain::ports::NoopMetrics),
            None,
        );
        let ctx = test_ctx();
        let auth = AuthConfig {
            plugin_type: gts_id!("cf.core.oagw.auth_plugin.v1~cf.core.oagw.missing.v1").into(),
            sharing: SharingMode::Private,
            config: None,
        };
        let (upstream, _) = apikey_target("cred://k");
        match execute_auth_plugin(
            &svc.auth_registry,
            &upstream,
            &auth,
            &mut HeaderMap::new(),
            &ctx,
            "/i",
        )
        .await
        .map(|_| ())
        {
            Err(DomainError::AuthenticationFailed { reason: r, .. }) => {
                assert_eq!(r, reason::auth::PLUGIN_NOT_FOUND);
            }
            other => panic!("expected AUTH_PLUGIN_NOT_FOUND, got {other:?}"),
        }

        let binding = |plugin_ref: &str| PluginBinding {
            plugin_ref: plugin_ref.into(),
            config: Default::default(),
        };
        let guard = binding(gts_id!(
            "cf.core.oagw.guard_plugin.v1~cf.core.oagw.missing.v1"
        ));
        let refused = execute_guard_requests(
            &svc.guard_registry,
            &[&guard],
            &HeaderMap::new(),
            "GET",
            "/",
            &ctx,
            "/i",
        )
        .await;
        assert!(
            matches!(refused, Err(DomainError::Internal { .. })),
            "{refused:?}"
        );

        let transform = binding(gts_id!(
            "cf.core.oagw.transform_plugin.v1~cf.core.oagw.missing.v1"
        ));
        let mut headers = HeaderMap::new();
        headers.insert("x-a", HeaderValue::from_static("1"));
        let mut query = vec![("q".to_owned(), "1".to_owned())];
        execute_transform_requests(
            &svc.transform_registry,
            &[&transform],
            &mut headers,
            &mut query,
            "GET",
            "/",
            &ctx,
        )
        .await;
        assert_eq!(headers.len(), 1);
        assert_eq!(headers["x-a"], "1");
        assert_eq!(query, [("q".to_owned(), "1".to_owned())]);

        // A short id with no schema prefix (`audit-log`, accepted before the
        // write check) is neither a guard nor a transform: it is skipped.
        let mut upstream = upstream_with(vec![]);
        upstream.plugins = Some(crate::domain::model::PluginsConfig {
            sharing: SharingMode::Private,
            items: vec![
                binding("audit-log"),
                binding(crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID),
            ],
        });
        assert!(
            collect_plugin_bindings(&upstream, GuardPluginRegistry::is_guard_plugin).is_empty()
        );
        let transforms =
            collect_plugin_bindings(&upstream, TransformPluginRegistry::is_transform_plugin);
        let refs: Vec<&str> = transforms.iter().map(|b| b.plugin_ref.as_str()).collect();
        assert_eq!(
            refs,
            [crate::domain::gts_helpers::REQUEST_ID_TRANSFORM_PLUGIN_ID],
            "the real binding still runs"
        );
    }

    /// A `proxy:invoke` check dropped before the policy engine answers (the
    /// gateway's request timeout) is counted as a cancelled request.
    #[tokio::test]
    async fn cancelled_authorization_is_counted() {
        use authz_resolver_sdk::{AuthZResolverApi, EvaluationRequest, EvaluationResponse};
        struct NeverAnswers;
        #[async_trait]
        impl AuthZResolverApi for NeverAnswers {
            async fn evaluate(
                &self,
                _: toolkit_security::PlatformSecurityContext,
                _: EvaluationRequest,
            ) -> Result<EvaluationResponse, toolkit_canonical_errors::CanonicalError> {
                std::future::pending().await
            }
        }
        let metrics = Arc::new(RequestRecorder::default());
        let mut svc =
            build_svc_with_decision(Arc::new(MockSelector::new()), metrics.clone(), None, true);
        svc.policy_enforcer = PolicyEnforcer::new(Arc::new(NeverAnswers));
        let timed_out = tokio::time::timeout(
            Duration::from_millis(20),
            svc.authorize_proxy(test_ctx(), &http::Method::GET),
        )
        .await;
        assert!(timed_out.is_err());
        // No status: the client got no response.
        assert_eq!(
            metrics.events(),
            ["duration total", "error unknown Cancelled"]
        );
    }

    /// A grant dropped before it is proxied or rejected counts its request
    /// as `Cancelled`, with no status; one handed to the proxy or to
    /// `reject_granted` counts nothing more on drop.
    #[tokio::test]
    async fn a_dropped_grant_is_counted_as_cancelled() {
        let metrics = Arc::new(RequestRecorder::default());
        let svc =
            build_svc_with_decision(Arc::new(MockSelector::new()), metrics.clone(), None, true);
        let grant = svc
            .authorize_proxy(test_ctx(), &http::Method::GET)
            .await
            .unwrap();
        drop(grant);
        assert_eq!(
            metrics.events(),
            ["duration total", "error unknown Cancelled"]
        );

        let metrics = Arc::new(RequestRecorder::default());
        let svc =
            build_svc_with_decision(Arc::new(MockSelector::new()), metrics.clone(), None, true);
        let grant = svc
            .authorize_proxy(test_ctx(), &http::Method::POST)
            .await
            .unwrap();
        svc.reject_granted(
            grant,
            &DomainError::PayloadTooLarge {
                detail: "too large".into(),
                instance: String::new(),
            },
        );
        assert_eq!(
            metrics.events(),
            [
                "duration total",
                "request unknown POST 413",
                "error unknown PayloadTooLarge",
            ]
        );

        // A grant without a count (the SDK path, tests) counts nothing.
        drop(ProxyGrant::for_tests(test_ctx()));
    }

    /// A gateway error is counted in `requests_total` with the status the
    /// client gets, next to `errors_total`. The request is in flight from
    /// the start, under `unknown` until the upstream is resolved (#4989).
    #[tokio::test]
    async fn gateway_errors_are_counted_with_their_status() {
        let metrics = Arc::new(RequestRecorder::default());
        let svc = build_svc_resolving(Arc::new(MockSelector::new()), metrics.clone(), None);
        let err = svc
            .proxy_request(test_ctx(), proxy_get("/missing/v1"))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::NotFound { .. }), "{err:?}");
        assert_eq!(
            metrics.events(),
            [
                "in_flight +1 unknown",
                "duration resolve",
                "in_flight -1 unknown",
                "duration total",
                "request unknown GET 404",
                "error unknown NotFound",
            ]
        );

        // Failing after resolution: counted under the upstream's alias.
        let (upstream, route) = local_target(1);
        let metrics = Arc::new(RequestRecorder::default());
        let svc = build_svc_resolving(
            Arc::new(MockSelector::new()),
            metrics.clone(),
            Some((upstream, route)),
        );
        let err = svc
            .proxy_request(test_ctx(), proxy_get("/test/v1?x=1"))
            .await
            .unwrap_err();
        assert!(matches!(err, DomainError::Validation { .. }), "{err:?}");
        assert_eq!(
            metrics.events(),
            [
                "in_flight +1 unknown",
                "in_flight +1 test",
                "in_flight -1 unknown",
                "duration resolve",
                "in_flight -1 test",
                "duration total",
                "request test GET 400",
                "error test ValidationError",
            ]
        );
    }

    /// An HTTP upstream on 127.0.0.1:`port` with a `GET /v1` route.
    fn local_target(port: u16) -> (Upstream, crate::domain::model::Route) {
        use crate::domain::model::{HttpMatch, HttpMethod, MatchRules, Route};
        let upstream = upstream_with(vec![Endpoint {
            scheme: Scheme::Http,
            host: "127.0.0.1".into(),
            port,
        }]);
        let route = Route {
            id: Uuid::new_v4(),
            tenant_id: upstream.tenant_id,
            upstream_id: upstream.id,
            match_rules: MatchRules {
                http: Some(HttpMatch {
                    methods: vec![HttpMethod::Get],
                    path: "/v1".into(),
                    query_allowlist: vec![],
                    path_suffix_mode: PathSuffixMode::Append,
                }),
                grpc: None,
            },
            plugins: None,
            rate_limit: None,
            cors: None,
            tags: vec![],
            priority: 0,
            enabled: true,
            managed_by: ManagedBy::Api,
        };
        (upstream, route)
    }

    /// A one-request HTTP/1.1 upstream on 127.0.0.1. After the request head
    /// it writes each part sent on the returned channel, and closes the
    /// connection once the channel closes.
    async fn raw_upstream() -> (u16, tokio::sync::mpsc::UnboundedSender<&'static [u8]>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<&'static [u8]>();
        tokio::spawn(async move {
            let Ok((mut tcp, _)) = listener.accept().await else {
                return;
            };
            let mut head = Vec::new();
            let mut buf = [0u8; 1024];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match tcp.read(&mut buf).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => head.extend_from_slice(&buf[..n]),
                }
            }
            while let Some(part) = rx.recv().await {
                if tcp.write_all(part).await.is_err() {
                    return;
                }
            }
            let _ = tcp.shutdown().await;
        });
        (port, tx)
    }

    /// An HTTP/1.1 upstream on 127.0.0.1 that reads each request head and
    /// closes the connection without a response. It keeps listening, so a
    /// retry cannot reach another test's listener on a reused port.
    async fn closing_upstream() -> u16 {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match tcp.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
            }
        });
        port
    }

    /// A service proxying `/test/v1` to the raw upstream on `port`.
    fn local_svc(port: u16, metrics: Arc<dyn OagwMetricsPort>) -> DataPlaneServiceImpl {
        build_svc_resolving(
            Arc::new(MockSelector::new()),
            metrics,
            Some(local_target(port)),
        )
        .with_allow_http_upstream(true)
    }

    /// An endpoint selection that does not answer by the request deadline
    /// is a 504 that tells the client nothing was sent
    /// (`REQUEST_NOT_SENT`), so any method may be retried.
    #[tokio::test]
    async fn a_hung_endpoint_selection_times_out_as_not_sent() {
        use oagw_sdk::ServiceGatewayError;
        use oagw_sdk::reason::timeout::TimeoutReason;
        use toolkit_canonical_errors::CanonicalError;

        struct Hangs;
        #[async_trait]
        impl EndpointSelector for Hangs {
            async fn select(
                &self,
                _upstream_id: Uuid,
                _endpoints: &[Endpoint],
            ) -> Result<SelectedEndpoint, SelectionError> {
                std::future::pending().await
            }

            fn invalidate(&self, _upstream_id: Uuid) {}
        }

        // Two endpoints, so the selector is asked.
        let (mut upstream, route) = local_target(1);
        upstream.server.endpoints.push(Endpoint {
            scheme: Scheme::Http,
            host: "127.0.0.2".into(),
            port: 1,
        });
        let svc = build_svc_resolving(
            Arc::new(Hangs),
            Arc::new(crate::domain::ports::NoopMetrics),
            Some((upstream, route)),
        )
        .with_allow_http_upstream(true)
        .with_request_timeout(Duration::from_millis(50));
        let err = svc
            .proxy_request(test_ctx(), proxy_get("/test/v1"))
            .await
            .unwrap_err();
        assert!(
            matches!(
                &err,
                DomainError::RequestTimeout {
                    dispatch: Dispatch::NotSent,
                    ..
                }
            ),
            "{err:?}"
        );
        let canonical = CanonicalError::from(err);
        assert_eq!(canonical.status_code(), 504);
        assert!(
            matches!(
                ServiceGatewayError::from(canonical),
                ServiceGatewayError::Timeout {
                    reason: Some(TimeoutReason::RequestNotSent)
                }
            ),
            "selection timeout"
        );
    }

    const CHUNKED_HEAD: &[u8] =
        b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n";

    /// The events recorded when the response head of a resolved `GET` came
    /// back with `status`, its body still open.
    fn head_events(status: u16) -> Vec<String> {
        [
            "in_flight +1 unknown",
            "in_flight +1 test",
            "in_flight -1 unknown",
            "duration resolve",
            "duration total",
        ]
        .into_iter()
        .map(str::to_owned)
        .chain([format!("request test GET {status}")])
        .collect()
    }

    fn body_stream(resp: http::Response<Body>) -> BodyStream {
        match resp.into_body() {
            Body::Stream(stream) => stream,
            other => panic!("expected a streamed body, got {other:?}"),
        }
    }

    /// A request stays in flight until its body ends, and the stream
    /// duration is recorded then, as `completed`; the duration to the head
    /// and `requests_total` are recorded when the head returns (#4989, #4987).
    #[tokio::test]
    async fn in_flight_lasts_until_the_body_ends() {
        let (port, upstream) = raw_upstream().await;
        let metrics = Arc::new(RequestRecorder::default());
        let svc = local_svc(port, metrics.clone());
        upstream.send(CHUNKED_HEAD).unwrap();

        let resp = svc
            .proxy_request(test_ctx(), proxy_get("/test/v1"))
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::OK);
        let mut body = body_stream(resp);
        assert_eq!(body.next().await.unwrap().unwrap(), "hello");
        assert_eq!(metrics.events(), head_events(200));

        upstream.send(b"0\r\n\r\n").unwrap();
        assert!(body.next().await.is_none());
        let mut expected = head_events(200);
        expected.extend([
            "stream test completed".to_owned(),
            "in_flight -1 test".to_owned(),
        ]);
        assert_eq!(metrics.events(), expected);
        drop(body);
        assert_eq!(metrics.events(), expected, "recorded once");
    }

    /// A body the upstream cuts ends as `error`; an SSE stream the gateway's
    /// stop ends as `shutdown`; a body dropped before its end (the client
    /// went away) as `cancelled`. Each leaves the in-flight gauge then
    /// (#4987).
    #[tokio::test]
    async fn stream_outcome_is_recorded_when_the_body_ends() {
        // The upstream closes the connection mid-body.
        let (port, upstream) = raw_upstream().await;
        let metrics = Arc::new(RequestRecorder::default());
        let svc = local_svc(port, metrics.clone());
        upstream.send(CHUNKED_HEAD).unwrap();
        let resp = svc
            .proxy_request(test_ctx(), proxy_get("/test/v1"))
            .await
            .unwrap();
        let mut body = body_stream(resp);
        assert_eq!(body.next().await.unwrap().unwrap(), "hello");
        drop(upstream);
        assert!(matches!(body.next().await, Some(Err(_))));
        let mut expected = head_events(200);
        expected.extend([
            "stream test error".to_owned(),
            "in_flight -1 test".to_owned(),
        ]);
        assert_eq!(metrics.events(), expected);

        // The gateway stops under an open SSE stream.
        let (port, upstream) = raw_upstream().await;
        let metrics = Arc::new(RequestRecorder::default());
        let svc = local_svc(port, metrics.clone());
        upstream
            .send(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                  transfer-encoding: chunked\r\n\r\nb\r\ndata: one\n\n\r\n",
            )
            .unwrap();
        let resp = svc
            .proxy_request(test_ctx(), proxy_get("/test/v1"))
            .await
            .unwrap();
        let mut body = body_stream(resp);
        assert_eq!(body.next().await.unwrap().unwrap(), "data: one\n\n");
        svc.stream_shutdown().end_streams();
        crate::infra::proxy::streaming::tests::assert_shutdown_error(body.next().await);
        let mut expected = head_events(200);
        expected.extend([
            "stream test shutdown".to_owned(),
            "in_flight -1 test".to_owned(),
        ]);
        assert_eq!(metrics.events(), expected);

        // The client goes away before the body ends.
        let (port, upstream) = raw_upstream().await;
        let metrics = Arc::new(RequestRecorder::default());
        let svc = local_svc(port, metrics.clone());
        upstream.send(CHUNKED_HEAD).unwrap();
        let resp = svc
            .proxy_request(test_ctx(), proxy_get("/test/v1"))
            .await
            .unwrap();
        drop(resp);
        let mut expected = head_events(200);
        expected.extend([
            "stream test cancelled".to_owned(),
            "in_flight -1 test".to_owned(),
        ]);
        assert_eq!(metrics.events(), expected);
    }

    /// A response without a body (204, 304, an answer to HEAD) ends at its
    /// head and records no stream duration: hyper never reads its body,
    /// which would otherwise end as `cancelled`.
    #[tokio::test]
    async fn a_response_without_a_body_ends_at_its_head() {
        let cases: [(http::Method, &'static [u8], u16); 3] = [
            (http::Method::GET, b"HTTP/1.1 204 No Content\r\n\r\n", 204),
            (http::Method::GET, b"HTTP/1.1 304 Not Modified\r\n\r\n", 304),
            (
                http::Method::HEAD,
                b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\n",
                200,
            ),
        ];
        for (method, head, status) in cases {
            let (port, upstream) = raw_upstream().await;
            let metrics = Arc::new(RequestRecorder::default());
            let svc = local_svc(port, metrics.clone());
            upstream.send(head).unwrap();
            let req = http::Request::builder()
                .method(method.clone())
                .uri("/test/v1")
                .body(Body::Empty)
                .unwrap();
            let resp = svc.proxy_request(test_ctx(), req).await.unwrap();
            assert_eq!(resp.status().as_u16(), status);
            let expected = [
                "in_flight +1 unknown".to_owned(),
                "in_flight +1 test".to_owned(),
                "in_flight -1 unknown".to_owned(),
                "duration resolve".to_owned(),
                "in_flight -1 test".to_owned(),
                "duration total".to_owned(),
                format!("request test {method} {status}"),
            ];
            assert_eq!(metrics.events(), expected, "{method} {status}");
            drop(resp);
            assert_eq!(metrics.events(), expected, "nothing more at the drop");
        }
    }

    /// A request dropped after resolution, before the response head (the
    /// client went away), leaves the in-flight gauge balanced and counts
    /// only as `Cancelled`: no status, as the client got none (#4989).
    #[tokio::test]
    async fn a_request_dropped_before_its_head_is_cancelled() {
        // The upstream reads the request and never answers.
        let (port, _upstream) = raw_upstream().await;
        let metrics = Arc::new(RequestRecorder::default());
        let svc = local_svc(port, metrics.clone());
        let timed_out = tokio::time::timeout(
            Duration::from_millis(50),
            svc.proxy_request(test_ctx(), proxy_get("/test/v1")),
        )
        .await;
        assert!(timed_out.is_err());
        assert_eq!(
            metrics.events(),
            [
                "in_flight +1 unknown",
                "in_flight +1 test",
                "in_flight -1 unknown",
                "duration resolve",
                "in_flight -1 test",
                "duration total",
                "error test Cancelled",
            ]
        );
    }

    /// A gateway recording into `metrics`, with the upstream `test` on
    /// 127.0.0.1:`port` and its `GET /v1` route, served over HTTP/1.1 by
    /// hyper on the returned address, so requests go through the REST
    /// handler as in production.
    async fn served_gateway(
        port: u16,
        metrics: Arc<RequestRecorder>,
    ) -> (crate::test_support::AppHarness, std::net::SocketAddr) {
        use oagw_sdk::{CreateRouteRequest, CreateUpstreamRequest, HttpMatch, MatchRules};
        let h = crate::test_support::AppHarness::builder()
            .with_metrics(metrics)
            .with_max_body_size(8)
            .build()
            .await;
        let ctx = h.security_context().clone();
        let upstream = h
            .facade()
            .create_upstream(
                ctx.clone(),
                CreateUpstreamRequest::builder(
                    oagw_sdk::Server {
                        endpoints: vec![oagw_sdk::Endpoint {
                            scheme: oagw_sdk::Scheme::Http,
                            host: "127.0.0.1".into(),
                            port,
                        }],
                    },
                    oagw_sdk::HTTP_PROTOCOL_ID,
                )
                .alias("test")
                .build(),
            )
            .await
            .unwrap();
        h.facade()
            .create_route(
                ctx,
                CreateRouteRequest::builder(
                    upstream.id,
                    MatchRules {
                        http: Some(HttpMatch {
                            methods: vec![oagw_sdk::HttpMethod::Get, oagw_sdk::HttpMethod::Post],
                            path: "/v1".into(),
                            query_allowlist: vec![],
                            path_suffix_mode: oagw_sdk::PathSuffixMode::Append,
                        }),
                        grpc: None,
                    },
                )
                .build(),
            )
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = h.router().clone();
        tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });
        (h, addr)
    }

    /// Sends `req` over HTTP/1.1 to `addr`: the response status and its
    /// whole body.
    async fn http1_send(
        addr: std::net::SocketAddr,
        req: http::Request<http_body_util::Full<Bytes>>,
    ) -> (http::StatusCode, Bytes) {
        use http_body_util::BodyExt as _;
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tcp))
                .await
                .unwrap();
        tokio::spawn(conn);
        let resp = sender.send_request(req).await.unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (status, body)
    }

    /// The events once there are at least `n`; hyper ends the response body
    /// after the client may have read it.
    async fn events_after(metrics: &RequestRecorder, n: usize) -> Vec<String> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while metrics.events().len() < n {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("fewer than {n} events: {:?}", metrics.events()));
        metrics.events()
    }

    /// hyper's HTTP/1.1 server stops polling a Content-Length body once it
    /// has written the declared bytes and drops it, without asking for its
    /// end. Such a body still ends as `completed` (#4987).
    #[tokio::test]
    async fn a_content_length_body_served_by_hyper_ends_as_completed() {
        let (port, upstream) = raw_upstream().await;
        let metrics = Arc::new(RequestRecorder::default());
        let (_h, addr) = served_gateway(port, metrics.clone()).await;
        upstream
            .send(b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\n\r\nhello")
            .unwrap();

        let req = http::Request::get("/oagw/v1/proxy/test/v1")
            .header(http::header::HOST, addr.to_string())
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();
        let (status, body) = http1_send(addr, req).await;
        assert_eq!(status, http::StatusCode::OK);
        assert_eq!(body, "hello");

        let mut expected = head_events(200);
        expected.extend([
            "stream test completed".to_owned(),
            "in_flight -1 test".to_owned(),
        ]);
        assert_eq!(events_after(&metrics, expected.len()).await, expected);
    }

    /// Requests the REST handler rejects after the `proxy:invoke` grant, a
    /// body over the limit and a path without an alias, are counted as the
    /// Data Plane counts its gateway errors: with the status the client
    /// gets, under the unknown host (#4989).
    #[tokio::test]
    async fn handler_rejections_after_the_grant_are_counted() {
        let metrics = Arc::new(RequestRecorder::default());
        // No request reaches the upstream.
        let (_h, addr) = served_gateway(1, metrics.clone()).await;

        let too_large = http::Request::post("/oagw/v1/proxy/test/v1")
            .header(http::header::HOST, addr.to_string())
            .body(http_body_util::Full::new(Bytes::from_static(
                b"longer than eight bytes",
            )))
            .unwrap();
        let (status, _) = http1_send(addr, too_large).await;
        assert_eq!(status, http::StatusCode::PAYLOAD_TOO_LARGE);
        let events = metrics.events();
        assert_eq!(
            events,
            [
                "duration total",
                "request unknown POST 413",
                "error unknown PayloadTooLarge",
            ]
        );

        let metrics = Arc::new(RequestRecorder::default());
        let (_h, addr) = served_gateway(1, metrics.clone()).await;
        let no_alias = http::Request::get("/oagw/v1/proxy//v1")
            .header(http::header::HOST, addr.to_string())
            .body(http_body_util::Full::new(Bytes::new()))
            .unwrap();
        let (status, _) = http1_send(addr, no_alias).await;
        assert_eq!(status, http::StatusCode::BAD_REQUEST);
        let events = metrics.events();
        assert_eq!(
            events,
            [
                "duration total",
                "request unknown GET 400",
                "error unknown ValidationError",
            ]
        );
    }

    /// Counts the transform phases it runs.
    #[derive(Default)]
    struct PhaseCounter {
        responses: AtomicUsize,
        errors: std::sync::Mutex<Vec<u16>>,
    }

    #[async_trait]
    impl crate::domain::plugin::TransformPlugin for PhaseCounter {
        async fn on_response(&self, _: &mut TransformResponseContext) -> Result<(), PluginError> {
            self.responses.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        async fn on_error(&self, ctx: &mut TransformErrorContext) -> Result<(), PluginError> {
            self.errors.lock().unwrap().push(ctx.status);
            Ok(())
        }
    }

    /// `local_svc` with the [`PhaseCounter`] transform bound to the upstream.
    fn svc_with_phase_counter(
        port: u16,
        metrics: Arc<dyn OagwMetricsPort>,
        counter: Arc<PhaseCounter>,
    ) -> DataPlaneServiceImpl {
        use crate::domain::model::{PluginBinding, PluginsConfig, SharingMode};
        let plugin_ref =
            toolkit_gts::gts_id!("cf.core.oagw.transform_plugin.v1~cf.core.oagw.phase_test.v1");
        let (mut upstream, route) = local_target(port);
        upstream.plugins = Some(PluginsConfig {
            sharing: SharingMode::Private,
            items: vec![PluginBinding {
                plugin_ref: plugin_ref.into(),
                config: Default::default(),
            }],
        });
        let mut svc = build_svc_resolving(
            Arc::new(MockSelector::new()),
            metrics,
            Some((upstream, route)),
        )
        .with_allow_http_upstream(true);
        svc.transform_registry.insert(plugin_ref, counter);
        svc
    }

    /// An upstream error response goes through `on_response`, not
    /// `on_error`, and is counted with its status, not as an error. A
    /// gateway error from the exchange runs `on_error` with the status the
    /// client gets (#4989).
    #[tokio::test]
    async fn on_error_runs_only_for_gateway_errors() {
        let (port, upstream) = raw_upstream().await;
        let metrics = Arc::new(RequestRecorder::default());
        let counter = Arc::new(PhaseCounter::default());
        let svc = svc_with_phase_counter(port, metrics.clone(), counter.clone());
        upstream
            .send(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 4\r\n\r\noops")
            .unwrap();
        let resp = svc
            .proxy_request(test_ctx(), proxy_get("/test/v1"))
            .await
            .unwrap();
        assert_eq!(resp.status(), http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(counter.responses.load(Ordering::SeqCst), 1);
        let errors = counter.errors.lock().unwrap().clone();
        assert!(errors.is_empty(), "{errors:?}");
        let events = metrics.events();
        assert!(
            events.contains(&"request test GET 500".to_owned()),
            "{events:?}"
        );
        assert!(!events.iter().any(|e| e.starts_with("error")), "{events:?}");

        // The upstream closes the connection without a response.
        let port = closing_upstream().await;
        let metrics = Arc::new(RequestRecorder::default());
        let counter = Arc::new(PhaseCounter::default());
        let svc = svc_with_phase_counter(port, metrics.clone(), counter.clone());
        let err = svc
            .proxy_request(test_ctx(), proxy_get("/test/v1"))
            .await
            .unwrap_err();
        let status = domain_error_status(&err);
        assert!(status >= 500, "{err:?}");
        assert_eq!(counter.responses.load(Ordering::SeqCst), 0);
        let errors = counter.errors.lock().unwrap().clone();
        assert_eq!(errors, [status]);
        let events = metrics.events();
        assert!(
            events.contains(&format!("request test GET {status}")),
            "{events:?}"
        );
        assert!(
            events.contains(&format!("error test {}", domain_error_type_name(&err))),
            "{events:?}"
        );
    }

    /// Each rate-limit bucket writes its own series: the route, the
    /// shadowed ancestor routes, the upstream and the shared pool. The
    /// shadowed routes share theirs, which gets the highest ratio among them
    /// once per request, not the last one checked (#4989).
    #[test]
    fn rate_limit_metrics_carry_the_bucket() {
        use crate::domain::model::{RateLimitAlgorithm, RateLimitScope, SharedPool, Window};
        let mut upstream = upstream_with(vec![]);
        let mut rl = limit(10, Window::Minute, RateLimitScope::Tenant);
        rl.shared_pool = Some(SharedPool {
            owner_id: Uuid::new_v4(),
            total: 5,
            window: Window::Minute,
            algorithm: RateLimitAlgorithm::TokenBucket,
        });
        upstream.rate_limit = Some(rl);
        let route = route_on(
            &upstream,
            Some(limit(4, Window::Minute, RateLimitScope::Tenant)),
        );
        // Root first: the root is checked last, the nearer ancestor first.
        let shadowed = [
            ShadowedRouteLimit {
                route_id: Uuid::new_v4(),
                rate_limit: limit(10, Window::Minute, RateLimitScope::Tenant),
            },
            ShadowedRouteLimit {
                route_id: Uuid::new_v4(),
                rate_limit: limit(2, Window::Minute, RateLimitScope::Tenant),
            },
        ];
        let ctx = test_ctx();
        let limiter = RateLimiter::new();
        let metrics = RequestRecorder::default();
        let check = || {
            check_rate_limits(
                &limiter, &upstream, &route, &shadowed, None, &ctx, "/t", &metrics, "h", "/p",
            )
        };

        check().unwrap();
        check().unwrap();
        assert!(matches!(
            check(),
            Err(DomainError::RateLimitExceeded { .. })
        ));
        assert_eq!(
            metrics.events(),
            [
                "usage route 0.25",
                "usage shadowed_route 0.5",
                "usage upstream 0.1",
                "usage pool 0.2",
                "usage route 0.5",
                "usage shadowed_route 1",
                "usage upstream 0.2",
                "usage pool 0.4",
                "usage route 0.75",
                "exceeded shadowed_route",
                "usage shadowed_route 1",
            ]
        );
    }

    /// The SDK path checks `proxy:invoke` before validating the request: a
    /// denied caller with an invalid Content-Type gets 403, not 400.
    #[tokio::test]
    async fn sdk_path_authorizes_before_validating() {
        let svc = build_svc_with_decision(
            Arc::new(MockSelector::new()),
            Arc::new(crate::domain::ports::NoopMetrics),
            None,
            false,
        );
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri("/alias/v1/x")
            .header(http::header::CONTENT_TYPE, "not a mime")
            .body(Body::from("body"))
            .unwrap();
        let err = svc.proxy_request(test_ctx(), req).await.unwrap_err();
        assert!(matches!(err, DomainError::Forbidden { .. }), "{err:?}");
    }

    /// Keeps only what the sweep reports.
    #[derive(Default)]
    struct SweepRecorder(std::sync::Mutex<Vec<(u64, u64)>>);

    impl OagwMetricsPort for SweepRecorder {
        fn record_request(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: u16,
            _: Option<crate::domain::plugin::credentials::CredentialMode>,
        ) {
        }
        fn record_error(
            &self,
            _: &str,
            _: &str,
            _: &str,
            _: Option<crate::domain::plugin::credentials::CredentialMode>,
        ) {
        }
        fn record_request_duration_seconds(&self, _: &str, _: &str, _: &str, _: f64) {}
        fn increment_in_flight(&self, _: &str) {}
        fn decrement_in_flight(&self, _: &str) {}
        fn record_rate_limit_exceeded(
            &self,
            _: &str,
            _: &str,
            _: crate::domain::ports::metric_labels::RateLimitBucket,
        ) {
        }
        fn record_rate_limit_usage_ratio(
            &self,
            _: &str,
            _: &str,
            _: crate::domain::ports::metric_labels::RateLimitBucket,
            _: f64,
        ) {
        }
        fn increment_active_websocket_sessions(&self, _: &str) {}
        fn decrement_active_websocket_sessions(&self, _: &str) {}
        fn record_websocket_session_duration_seconds(
            &self,
            _: &str,
            _: crate::domain::ports::metric_labels::StreamOutcome,
            _: f64,
        ) {
        }
        fn record_stream_duration_seconds(
            &self,
            _: &str,
            _: &str,
            _: crate::domain::ports::metric_labels::StreamOutcome,
            _: f64,
        ) {
        }
        fn record_rate_limit_sweep(&self, evicted: u64, live: u64) {
            self.0.lock().unwrap().push((evicted, live));
        }
        fn record_registry_provisioning_failure(&self, _: &str, _: &str) {}
        fn record_registry_provisioning_run(&self, _: &str) {}
        fn record_credential_withheld(&self, _: &str, _: &'static str) {}
        fn record_proxy_cache_lookup(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: crate::domain::ports::metric_labels::CacheLookup,
        ) {
        }
        fn record_proxy_cache_entries(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: u64,
        ) {
        }
        fn record_proxy_cache_evictions(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: crate::domain::ports::metric_labels::CacheEviction,
            _: u64,
        ) {
        }
        fn record_proxy_cache_change(&self, _: &str) {}
        fn record_proxy_cache_flush(&self, _: &str) {}
        fn record_proxy_cache_load_discarded(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
        ) {
        }
        fn record_proxy_cache_stale_served(
            &self,
            _: crate::domain::ports::metric_labels::CacheMap,
            _: crate::domain::ports::metric_labels::CacheStale,
        ) {
        }
        fn record_proxy_cache_publish_failures(&self, _: &str, _: u64) {}
    }

    /// The sweeper the gear spawns ticks every interval, reports evicted and
    /// live bucket counts, and stops when the gear's token is cancelled.
    #[tokio::test(start_paused = true)]
    async fn spawned_sweeper_reports_each_tick_and_stops_on_cancel() {
        let metrics = Arc::new(SweepRecorder::default());
        let svc = build_svc_with_metrics(Arc::new(MockSelector::new()), metrics.clone());
        let mut slow = fast_refill_config();
        slow.sustained.window = crate::domain::model::Window::Day;
        svc.rate_limiter
            .try_consume("refilled", &fast_refill_config(), "/test")
            .unwrap();
        svc.rate_limiter
            .try_consume("draining", &slow, "/test")
            .unwrap();
        // The limiter reads the real clock: let the fast bucket refill.
        std::thread::sleep(Duration::from_millis(5));
        let half = IDLE_SWEEP_INTERVAL / 2;
        let cancel = CancellationToken::new();
        svc.spawn_idle_sweeper(cancel.clone());

        // Ticks fire at 0 and at one interval; each sleep ends between ticks.
        tokio::time::sleep(half).await;
        tokio::time::sleep(IDLE_SWEEP_INTERVAL).await;
        assert_eq!(*metrics.0.lock().unwrap(), vec![(1, 1), (0, 1)]);

        cancel.cancel();
        tokio::time::sleep(IDLE_SWEEP_INTERVAL * 3).await;
        assert_eq!(metrics.0.lock().unwrap().len(), 2, "no tick after cancel");
    }

    /// The same sweep evicts circuit breakers idle past their timeout.
    #[tokio::test(start_paused = true)]
    async fn sweep_evicts_idle_circuit_breakers() {
        let mut upstream = upstream_with(vec![ep("10.0.0.1", 443)]);
        upstream.circuit_breaker = Some(crate::domain::model::CircuitBreakerConfig {
            sharing: Default::default(),
            enabled: true,
            failure_threshold: 1,
            timeout_seconds: 1,
            success_threshold: 1,
            owner_id: None,
        });
        let limiter = Arc::new(RateLimiter::new());
        let breakers = Arc::new(CircuitBreakers::new());
        breakers
            .admit(&upstream, "/")
            .unwrap()
            .answered(http::StatusCode::SERVICE_UNAVAILABLE);
        let sweep = tokio::spawn(sweep_idle_state(
            Arc::downgrade(&limiter),
            Arc::downgrade(&breakers),
            Arc::new(crate::domain::ports::NoopMetrics),
            IDLE_SWEEP_INTERVAL,
            CancellationToken::new(),
        ));
        tokio::time::sleep(IDLE_SWEEP_INTERVAL * 5).await;
        assert_eq!(breakers.len(), 1, "idle for less than the grace");
        tokio::time::sleep(IDLE_SWEEP_INTERVAL * 7).await;
        assert_eq!(breakers.len(), 0);
        sweep.abort();
    }

    #[tokio::test]
    async fn sweep_ends_when_the_limiter_is_dropped() {
        let limiter = Arc::new(RateLimiter::new());
        let sweep = tokio::spawn(sweep_idle_state(
            Arc::downgrade(&limiter),
            Weak::new(),
            Arc::new(crate::domain::ports::NoopMetrics),
            Duration::from_millis(5),
            CancellationToken::new(),
        ));

        drop(limiter);
        tokio::time::timeout(Duration::from_secs(5), sweep)
            .await
            .expect("the sweep ends")
            .unwrap();
    }
}
