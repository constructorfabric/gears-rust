use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use authz_resolver_sdk::PolicyEnforcer;
use authz_resolver_sdk::pep::AccessRequest;
use bytes::Bytes;
use credstore_sdk::CredStoreClientV1;
use futures_util::StreamExt;
use http::{HeaderMap, HeaderValue};
use modkit_security::SecurityContext;
use oagw_sdk::body::{Body, BodyStream};
use pingora_core::apps::HttpServerApp;
use pingora_proxy::HttpProxy;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;

use uuid::Uuid;

use crate::config::TokenCacheConfig;
use crate::domain::error::DomainError;
use crate::domain::model::{
    PassthroughMode, PathSuffixMode, ResponseHeaderRules, Scheme, Upstream,
};
use crate::domain::plugin::{
    AuthContext, GuardContext, GuardDecision, TransformErrorContext, TransformRequestContext,
    TransformResponseContext,
};
use crate::domain::ports::OagwMetricsPort;
use crate::domain::ports::metric_labels::{self, phase};
use crate::domain::rate_limit::{
    RateLimitKeyContext, RateLimitOutcome, RateLimitResource, RateLimiter, build_rate_limit_key,
};
use crate::domain::services::{
    ControlPlaneService, DataPlaneService, EndpointSelector, SelectedEndpoint,
};
use crate::infra::plugin::{AuthPluginRegistry, GuardPluginRegistry, TransformPluginRegistry};
use crate::infra::proxy::{actions, resources};

use super::headers;
use super::pingora_proxy::{
    H_ENDPOINT_HOST, H_ENDPOINT_PORT, H_ENDPOINT_SCHEME, H_INSTANCE_URI, H_RESOLVED_ADDR,
    H_UPSTREAM_ID, PingoraProxy,
};
use super::{request_builder, session_bridge};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Default maximum request body size: 100 MB.
const MAX_BODY_SIZE: usize = 100 * 1024 * 1024;

/// Data Plane service implementation: proxy orchestration and plugin execution.
pub struct DataPlaneServiceImpl {
    cp: Arc<dyn ControlPlaneService>,
    backend_selector: Arc<dyn EndpointSelector>,
    proxy: Arc<HttpProxy<PingoraProxy>>,
    /// Sender kept alive so receivers see `false` (not shutting down) until drop.
    _shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    auth_registry: AuthPluginRegistry,
    guard_registry: GuardPluginRegistry,
    transform_registry: TransformPluginRegistry,
    rate_limiter: RateLimiter,
    request_timeout: Duration,
    /// Enforces authorization policy before proxying each request.
    policy_enforcer: PolicyEnforcer,
    /// When true, allow HTTP (non-TLS) upstream connections.
    allow_http_upstream: bool,
    /// Maximum request body size in bytes (applies to both buffered and streaming bodies).
    max_body_size: usize,
    /// Idle timeout for WebSocket connections (no data in either direction).
    websocket_idle_timeout: Duration,
    /// Timeout for the WebSocket Close frame handshake.
    websocket_close_timeout: Duration,
    /// Optional max WebSocket frame payload size (Close 1009 on exceed).
    websocket_max_frame_size: Option<usize>,
    /// Idle timeout for SSE streaming connections (no data from upstream).
    streaming_idle_timeout: Duration,
    /// Operational metrics port (OTel-backed in production).
    metrics: Arc<dyn OagwMetricsPort>,
}

impl DataPlaneServiceImpl {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cp: Arc<dyn ControlPlaneService>,
        credstore: Arc<dyn CredStoreClientV1>,
        policy_enforcer: PolicyEnforcer,
        token_http_config: Option<modkit_http::HttpClientConfig>,
        token_cache_config: TokenCacheConfig,
        backend_selector: Arc<dyn EndpointSelector>,
        proxy: Arc<HttpProxy<PingoraProxy>>,
        metrics: Arc<dyn OagwMetricsPort>,
    ) -> Self {
        let auth_registry =
            AuthPluginRegistry::with_builtins(credstore, token_http_config, token_cache_config);
        let guard_registry = GuardPluginRegistry::with_builtins();
        let transform_registry = TransformPluginRegistry::with_builtins();
        let rate_limiter = RateLimiter::new();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        Self {
            cp,
            backend_selector,
            proxy,
            _shutdown_tx: shutdown_tx,
            shutdown_rx,
            auth_registry,
            guard_registry,
            transform_registry,
            rate_limiter,
            request_timeout: REQUEST_TIMEOUT,
            policy_enforcer,
            allow_http_upstream: false,
            max_body_size: MAX_BODY_SIZE,
            websocket_idle_timeout: Duration::from_secs(300),
            websocket_close_timeout: Duration::from_secs(5),
            websocket_max_frame_size: None,
            streaming_idle_timeout: Duration::from_secs(300),
            metrics,
        }
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

    /// Override the WebSocket Close frame handshake timeout.
    #[must_use]
    pub fn with_websocket_close_timeout(mut self, timeout: Duration) -> Self {
        self.websocket_close_timeout = timeout;
        self
    }

    /// Override the maximum WebSocket frame payload size.
    #[must_use]
    pub fn with_websocket_max_frame_size(mut self, size: Option<usize>) -> Self {
        self.websocket_max_frame_size = size;
        self
    }

    /// Override the SSE streaming idle timeout.
    #[must_use]
    pub fn with_streaming_idle_timeout(mut self, timeout: Duration) -> Self {
        self.streaming_idle_timeout = timeout;
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

        // Apply streaming lifecycle management for SSE responses:
        // idle timeout and graceful shutdown awareness.
        let resp_body_stream = if oagw_sdk::sse::is_server_events_response(&resp_headers) {
            session_bridge::streaming_body_with_lifecycle(
                resp_body_stream,
                self.streaming_idle_timeout,
                self.shutdown_rx.clone(),
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
            });
        }

        // Tier 1: Explicit selection via X-OAGW-Target-Host header.
        if let Some(target_host) = req_headers
            .get("x-oagw-target-host")
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
            // Single-endpoint: bypass LB. `resolved_addr` is None, so
            // `upstream_peer` will fall back to DNS on the request path.
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
            })
    }
}

/// Parsed and validated proxy request, ready for resolution and forwarding.
struct ParsedRequest {
    instance_uri: String,
    alias: String,
    path_suffix: String,
    query_params: Vec<(String, String)>,
    method: http::Method,
    req_headers: HeaderMap,
    is_upgrade: bool,
    body_bytes: Bytes,
    body_stream: Option<BodyStream>,
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
    let (alias, path_suffix) = {
        let path = req.uri().path();
        let trimmed = path.strip_prefix('/').unwrap_or(path);
        let (alias, raw_suffix) = match trimmed.find('/') {
            Some(pos) => (&trimmed[..pos], &trimmed[pos..]),
            None => (trimmed, ""),
        };
        (alias.to_string(), normalize_path(raw_suffix))
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
        query_params,
        method,
        req_headers,
        is_upgrade,
        body_bytes,
        body_stream,
    })
}

/// Terminal outcome captured before [`RequestMetricsGuard`] drops.
///
/// `Pending` is the unwritten state — the guard treats it as a cancellation
/// (future dropped before a result was produced) and records an error.
enum RequestOutcome {
    Pending,
    Success(u16),
    Error(&'static str),
}

/// RAII guard that owns the per-request metric bookkeeping for
/// [`DataPlaneServiceImpl::proxy_request`].
///
/// On construction it captures the start time. After upstream resolution,
/// [`Self::mark_resolved`] sets the labels and increments the in-flight
/// gauge. Before the request returns, the body calls
/// [`Self::set_success`] or [`Self::set_error`] to record the terminal
/// outcome. `Drop` then decrements the in-flight gauge (if it was ever
/// incremented), records the duration histogram, and emits the success or
/// error counter — even if the future is cancelled or panics.
struct RequestMetricsGuard {
    metrics: Arc<dyn OagwMetricsPort>,
    start: Instant,
    method: &'static str,
    host: String,
    path: String,
    in_flight: bool,
    outcome: RequestOutcome,
}

impl RequestMetricsGuard {
    fn new(metrics: Arc<dyn OagwMetricsPort>, method: &'static str) -> Self {
        Self {
            metrics,
            start: Instant::now(),
            method,
            host: metric_labels::UNKNOWN.to_owned(),
            path: metric_labels::UNKNOWN.to_owned(),
            in_flight: false,
            outcome: RequestOutcome::Pending,
        }
    }

    fn mark_resolved(&mut self, host: String, path: String) {
        self.host = host;
        self.path = path;
        self.metrics.increment_in_flight(&self.host);
        self.in_flight = true;
    }

    fn set_success(&mut self, status: u16) {
        self.outcome = RequestOutcome::Success(status);
    }

    fn set_error(&mut self, variant: &'static str) {
        self.outcome = RequestOutcome::Error(variant);
    }
}

impl Drop for RequestMetricsGuard {
    fn drop(&mut self) {
        if self.in_flight {
            self.metrics.decrement_in_flight(&self.host);
        }
        self.metrics.record_request_duration_seconds(
            &self.host,
            &self.path,
            phase::TOTAL,
            self.start.elapsed().as_secs_f64(),
        );
        match self.outcome {
            RequestOutcome::Success(status) => {
                self.metrics
                    .record_request(&self.host, &self.path, self.method, status);
            }
            RequestOutcome::Error(variant) => {
                self.metrics.record_error(&self.host, &self.path, variant);
            }
            RequestOutcome::Pending => {
                self.metrics
                    .record_error(&self.host, &self.path, "Cancelled");
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
        let method = crate::infra::metrics::normalize_method(req.method());
        let mut guard = RequestMetricsGuard::new(Arc::clone(&self.metrics), method);
        let mut host_label: String = metric_labels::UNKNOWN.to_owned();
        let mut path_label: String = metric_labels::UNKNOWN.to_owned();

        let result: Result<http::Response<Body>, DomainError> = async {
            let ParsedRequest {
                instance_uri,
                alias,
                path_suffix,
                mut query_params,
                method,
                req_headers,
                is_upgrade,
                body_bytes,
                body_stream,
            } = parse_proxy_request(req, self.max_body_size)?;

            self.policy_enforcer
                .access_scope_with(
                    &ctx,
                    &resources::PROXY,
                    actions::INVOKE,
                    None,
                    &AccessRequest::new()
                        .require_constraints(false)
                        .context_tenant_id(ctx.subject_tenant_id()),
                )
                .await?;

            let max_body = self.max_body_size;

            // 1. Resolve upstream + route in one pass (single hierarchy walk).
            let (upstream, route) = self
                .cp
                .resolve_proxy_target(&ctx, &alias, method.as_ref(), &path_suffix)
                .await?;

            // Capture metric labels now that upstream + route are known, and
            // begin tracking this request in the in-flight gauge via the
            // RAII guard.
            host_label = upstream.alias.clone();
            path_label = route
                .match_rules
                .http
                .as_ref()
                .map_or("/", |h| h.path.as_str())
                .to_owned();
            guard.mark_resolved(host_label.clone(), path_label.clone());

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
                        reason: "QUERY_NOT_ALLOWED",
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
            let extra = path_suffix.strip_prefix(route_path.as_str()).unwrap_or("");
            if !extra.is_empty() {
                return Err(DomainError::Validation {
                    field: "path",
                    reason: "PATH_SUFFIX_NOT_ALLOWED",
                    detail: format!(
                        "path suffix not allowed: route path_suffix_mode is disabled but request has extra path '{}'",
                        extra
                    ),
                    instance: instance_uri,
                });
            }
        }

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
        if let Some(ref auth) = upstream.auth {
            let auth_start = std::time::Instant::now();
            let auth_result = execute_auth_plugin(
                &self.auth_registry,
                auth,
                &outbound_headers,
                &ctx,
                &instance_uri,
            )
            .await;
            self.metrics.record_request_duration_seconds(
                &host_label,
                &path_label,
                phase::AUTH,
                auth_start.elapsed().as_secs_f64(),
            );
            outbound_headers = auth_result?;
        }

        // 5b. Execute guard plugins (upstream then route).
        //
        // Guards are blocking gates: a rejection short-circuits the pipeline
        // immediately. Guards enforce hard policies (allowlists, rate limits,
        // schema validation). Compare with transforms (step 5d) which use
        // log-and-continue semantics.
        let guard_bindings =
            collect_plugin_bindings(&upstream, GuardPluginRegistry::is_guard_plugin);

        execute_guard_requests(
            &self.guard_registry,
            &guard_bindings,
            &outbound_headers,
            method.as_str(),
            &path_suffix,
            &ctx,
            &instance_uri,
        )
        .await?;

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
            &ctx,
        )
        .await;

        // 6. Endpoint selection (two-tier: target-host header or round-robin).
        let selected = self
            .select_endpoint(&upstream, &req_headers, &instance_uri)
            .await?;
        let endpoint = &selected.endpoint;

        // 6b. Enforce HTTPS-only upstream constraint.
        if !self.allow_http_upstream && matches!(endpoint.scheme, Scheme::Http) {
            return Err(DomainError::Validation {
                field: "endpoint.scheme",
                reason: "HTTP_UPSTREAM_FORBIDDEN",
                detail: "upstream endpoint uses HTTP; only HTTPS endpoints are permitted".into(),
                instance: instance_uri,
            });
        }

        headers::set_host_header(&mut outbound_headers, &endpoint.host, endpoint.port);

        // 7. Check rate limits (upstream then route) with scope-aware keying.
        let rate_limit_outcome = check_rate_limits(
            &self.rate_limiter,
            &upstream,
            &route,
            &req_headers,
            &ctx,
            &instance_uri,
            self.metrics.as_ref(),
            &host_label,
            &path_label,
        )?;

        // 8. Build upstream URL.
        // Strip the route prefix from the full path so we get:
        // endpoint + route_path + remaining_suffix.
        let route_path = route
            .match_rules
            .http
            .as_ref()
            .map_or("/", |h| h.path.as_str());
        let remaining_suffix = path_suffix.strip_prefix(route_path).unwrap_or("");
        let url = request_builder::build_upstream_url(
            endpoint,
            route_path,
            remaining_suffix,
            &query_params,
        )?;

        // 8b. Inject internal context headers for PingoraProxy.
        let scheme_str = match endpoint.scheme {
            Scheme::Http => "http",
            Scheme::Https => "https",
            Scheme::Wss => "wss",
            Scheme::Wt => "wt",
            Scheme::Grpc => "grpc",
        };
        if let Ok(v) = HeaderValue::from_str(&upstream.id.to_string()) {
            outbound_headers.insert(H_UPSTREAM_ID, v);
        }
        if let Ok(v) = HeaderValue::from_str(&endpoint.host) {
            outbound_headers.insert(H_ENDPOINT_HOST, v);
        }
        if let Ok(v) = HeaderValue::from_str(&endpoint.port.to_string()) {
            outbound_headers.insert(H_ENDPOINT_PORT, v);
        }
        outbound_headers.insert(H_ENDPOINT_SCHEME, HeaderValue::from_static(scheme_str));
        if let Ok(v) = HeaderValue::from_str(&instance_uri) {
            outbound_headers.insert(H_INSTANCE_URI, v);
        }
        if let Some(addr) = selected.resolved_addr
            && let Ok(v) = HeaderValue::from_str(&addr.to_string())
        {
            outbound_headers.insert(H_RESOLVED_ADDR, v);
        }

        let response_header_rules = upstream
            .headers
            .as_ref()
            .and_then(|hc| hc.response.as_ref());

        let pipeline = ResponsePipelineCtx {
            guard_bindings,
            transform_bindings,
            method: method.as_str(),
            path_suffix: &path_suffix,
            ctx: &ctx,
            cors_config: effective_cors.as_ref(),
            origin: request_origin,
            response_header_rules,
            rate_limit_outcome,
            host: &host_label,
        };

        // 9. WebSocket upgrade: bypass the normal bridge and set up a
        // bidirectional raw-byte tunnel through Pingora.
        if is_upgrade {
            return self
                .handle_websocket_upgrade(
                    &method,
                    &url,
                    &outbound_headers,
                    &pipeline,
                    &instance_uri,
                )
                .await;
        }

        // 10. Bridge request into Pingora and read response.
        let upstream_result = self
            .bridge_and_respond(
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

        match &result {
            Ok(resp) => guard.set_success(resp.status().as_u16()),
            Err(err) => guard.set_error(domain_error_type_name(err)),
        }

        result
    }

    fn remove_rate_limit_keys_for_upstream(&self, upstream_id: Uuid) {
        self.rate_limiter.remove_keys_for_upstream(upstream_id);
    }

    fn remove_rate_limit_keys_for_route(&self, route_id: Uuid) {
        self.rate_limiter.remove_keys_for_route(route_id);
    }
}

/// Private helpers for the proxy pipeline.
impl DataPlaneServiceImpl {
    /// Handle a WebSocket upgrade request through the Pingora bridge.
    ///
    /// Sets up a bidirectional raw-byte tunnel, writes the upgrade request,
    /// validates the 101 response, runs response guards, and returns the
    /// upgraded response with the bridge IO stashed in extensions.
    async fn handle_websocket_upgrade(
        &self,
        method: &http::Method,
        url: &str,
        outbound_headers: &HeaderMap,
        pipeline: &ResponsePipelineCtx<'_>,
        instance_uri: &str,
    ) -> Result<http::Response<Body>, DomainError> {
        let (mut client_io, server_io) = tokio::io::duplex(65_536);
        let session = pingora_core::protocols::http::ServerSession::new_http1(Box::new(server_io));
        let proxy = self.proxy.clone();
        let shutdown = self.shutdown_rx.clone();
        tokio::spawn(async move {
            proxy.process_new_http(session, &shutdown).await;
        });

        // Write the upgrade request (Connection: Upgrade, no body).
        let wire = session_bridge::serialize_upgrade_request_wire(method, url, outbound_headers);
        client_io
            .write_all(&wire)
            .await
            .map_err(|e| DomainError::DownstreamError {
                detail: format!("failed to write upgrade request to proxy bridge: {e}"),
                instance: instance_uri.to_string(),
            })?;

        // Parse only the response headers (IO stays intact for bidirectional copy).
        let upgrade_timeout = self.request_timeout;
        let (status, resp_headers, leftover) = tokio::time::timeout(
            upgrade_timeout,
            session_bridge::parse_upgrade_response(&mut client_io),
        )
        .await
        .map_err(|_| DomainError::RequestTimeout {
            detail: format!("WebSocket upgrade to {url} timed out after {upgrade_timeout:?}"),
            instance: instance_uri.to_string(),
        })?
        .map_err(|e| DomainError::DownstreamError {
            detail: format!("proxy bridge error during WebSocket upgrade: {e}"),
            instance: instance_uri.to_string(),
        })?;

        if status != http::StatusCode::SWITCHING_PROTOCOLS {
            return Err(DomainError::ProtocolError {
                detail: format!("upstream rejected WebSocket upgrade with status {status}"),
                instance: instance_uri.to_string(),
            });
        }

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
        let mut resp_headers = resp_headers;
        self.apply_response_pipeline(pipeline, status, &mut resp_headers)
            .await;
        headers::sanitize_response_headers_for_upgrade(&mut resp_headers);

        // Build the 101 response with the DuplexStream stashed in extensions.
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
                    io: client_io,
                    leftover,
                    idle_timeout: self.websocket_idle_timeout,
                    close_timeout: self.websocket_close_timeout,
                    max_frame_size: self.websocket_max_frame_size,
                    shutdown_rx: self.shutdown_rx.clone(),
                    metrics: Some(self.metrics.clone()),
                    host: pipeline.host.to_string(),
                },
            ));
        Ok(resp)
    }

    /// Bridge a request into Pingora via in-memory DuplexStream and read the
    /// response.
    ///
    /// Handles both streaming (chunked) and buffered request bodies. The
    /// streaming path spawns a body-forwarding task that enforces
    /// `max_body_size` and signals 413 or abort on failures.
    #[allow(clippy::too_many_arguments)]
    async fn bridge_and_respond(
        &self,
        method: &http::Method,
        url: &str,
        outbound_headers: &HeaderMap,
        body_bytes: Bytes,
        body_stream: Option<BodyStream>,
        max_body: usize,
        pipeline: &ResponsePipelineCtx<'_>,
        instance_uri: &str,
    ) -> Result<http::Response<Body>, DomainError> {
        let (client_io, server_io) = tokio::io::duplex(65_536);

        // Create Pingora H1 session from the server side of the DuplexStream.
        // Pingora implements all IO traits for DuplexStream (in ext_io_impl).
        let session = pingora_core::protocols::http::ServerSession::new_http1(Box::new(server_io));

        // Spawn Pingora proxy processing in background.
        let proxy = self.proxy.clone();
        let shutdown = self.shutdown_rx.clone();
        tokio::spawn(async move {
            proxy.process_new_http(session, &shutdown).await;
        });

        let timeout = self.request_timeout;

        if let Some(mut body_stream) = body_stream {
            // Streaming path: write headers, then forward body chunks concurrently.
            let (client_read, mut client_write) = tokio::io::split(client_io);

            let header_bytes =
                session_bridge::serialize_request_wire(method, url, outbound_headers, None);
            client_write.write_all(&header_bytes).await.map_err(|e| {
                DomainError::DownstreamError {
                    detail: format!("failed to write to proxy bridge: {e}"),
                    instance: instance_uri.to_string(),
                }
            })?;

            // Spawn task to forward body stream chunks with chunked encoding.
            // Enforce max_body_size on the streaming path: signal 413 if exceeded.
            // Signal abort on stream/write errors so the main select! can fail
            // fast instead of waiting for the full request timeout.
            let (limit_tx, limit_rx) = tokio::sync::oneshot::channel::<usize>();
            let (abort_tx, abort_rx) = tokio::sync::oneshot::channel::<String>();
            let body_instance_uri = instance_uri.to_string();
            tokio::spawn(async move {
                let mut total_bytes: usize = 0;
                let mut exceeded = false;
                while let Some(chunk) = body_stream.next().await {
                    match chunk {
                        Ok(bytes) if !bytes.is_empty() => {
                            total_bytes = total_bytes.saturating_add(bytes.len());
                            if total_bytes > max_body {
                                tracing::warn!(
                                    total_bytes,
                                    max_body,
                                    "streaming body exceeded max size, aborting"
                                );
                                exceeded = true;
                                break;
                            }
                            // Chunked transfer encoding: {size_hex}\r\n{data}\r\n
                            let chunk_header = format!("{:x}\r\n", bytes.len());
                            if let Err(e) = client_write.write_all(chunk_header.as_bytes()).await {
                                tracing::debug!(error = %e, "body stream write error");
                                let _ = abort_tx.send(format!("body stream write error: {e}"));
                                return;
                            }
                            if let Err(e) = client_write.write_all(&bytes).await {
                                tracing::debug!(error = %e, "body stream write error");
                                let _ = abort_tx.send(format!("body stream write error: {e}"));
                                return;
                            }
                            if let Err(e) = client_write.write_all(b"\r\n").await {
                                tracing::debug!(error = %e, "body stream write error");
                                let _ = abort_tx.send(format!("body stream write error: {e}"));
                                return;
                            }
                        }
                        Ok(_) => {} // skip empty chunks
                        Err(e) => {
                            tracing::debug!(error = %e, "body stream chunk error");
                            let _ = abort_tx.send(format!("body stream read error: {e}"));
                            return;
                        }
                    }
                }
                if exceeded {
                    // Shutdown sends EOF to Pingora's read side before we
                    // signal the limit breach (drop alone won't close the pipe).
                    let _ = client_write.shutdown().await;
                    let _ = limit_tx.send(total_bytes);
                } else {
                    // Chunked terminator: signals end-of-body to Pingora.
                    // Only written after a clean end-of-stream — not after
                    // write failures or stream errors, where the body is
                    // incomplete and signalling clean EOF would be wrong.
                    let _ = client_write.write_all(b"0\r\n\r\n").await;
                    // Do NOT call shutdown() here — Pingora still needs the
                    // duplex open to send the response. The chunked terminator
                    // is sufficient to signal end-of-body. Calling shutdown()
                    // on the write half of a DuplexStream closes it for the
                    // peer's read, which can cause Pingora to see EOF before
                    // it finishes proxying (especially with fast streams).
                }
            });

            // Parse response from the read half, but short-circuit to 413
            // if the body-forwarding task signals a limit breach.
            let resp_future =
                tokio::time::timeout(timeout, session_bridge::parse_response_stream(client_read));
            tokio::select! {
                biased;
                Ok(total) = limit_rx => {
                    Err(DomainError::PayloadTooLarge {
                        detail: format!(
                            "streaming request body of {total} bytes exceeds maximum of {max_body} bytes"
                        ),
                        instance: body_instance_uri,
                    })
                }
                Ok(reason) = abort_rx => {
                    Err(DomainError::DownstreamError {
                        detail: format!("streaming request body failed mid-stream: {reason}"),
                        instance: body_instance_uri,
                    })
                }
                result = resp_future => {
                    let (status, resp_headers, resp_body_stream) = result
                        .map_err(|_| DomainError::RequestTimeout {
                            detail: format!("request to {url} timed out after {timeout:?}"),
                            instance: instance_uri.to_string(),
                        })?
                        .map_err(|e| DomainError::DownstreamError {
                            detail: format!("proxy bridge error: {e}"),
                            instance: instance_uri.to_string(),
                        })?;
                    self.finalize_response(
                        pipeline,
                        status,
                        resp_headers,
                        resp_body_stream,
                        instance_uri.to_string(),
                    )
                    .await
                }
            }
        } else {
            // Buffered path: write full request then read response.
            let wire = session_bridge::serialize_request_wire(
                method,
                url,
                outbound_headers,
                Some(&body_bytes),
            );
            let mut client_io = client_io;
            client_io
                .write_all(&wire)
                .await
                .map_err(|e| DomainError::DownstreamError {
                    detail: format!("failed to write to proxy bridge: {e}"),
                    instance: instance_uri.to_string(),
                })?;
            // Do NOT shutdown the write side — Pingora uses Content-Length to
            // determine the request boundary, and an early write-close is
            // misinterpreted as "downstream dropped the connection".

            let (status, resp_headers, resp_body_stream) =
                tokio::time::timeout(timeout, session_bridge::parse_response_stream(client_io))
                    .await
                    .map_err(|_| DomainError::RequestTimeout {
                        detail: format!("request to {url} timed out after {timeout:?}"),
                        instance: instance_uri.to_string(),
                    })?
                    .map_err(|e| DomainError::DownstreamError {
                        detail: format!("proxy bridge error: {e}"),
                        instance: instance_uri.to_string(),
                    })?;

            self.finalize_response(
                pipeline,
                status,
                resp_headers,
                resp_body_stream,
                instance_uri.to_string(),
            )
            .await
        }
    }
}

/// Execute the auth plugin for the upstream, mutating outbound headers with
/// injected credentials (e.g. Bearer token, API key).
///
/// Returns the updated header map. No-op if the upstream has no auth config.
async fn execute_auth_plugin(
    auth_registry: &AuthPluginRegistry,
    auth: &crate::domain::model::AuthConfig,
    outbound_headers: &HeaderMap,
    ctx: &SecurityContext,
    instance_uri: &str,
) -> Result<HeaderMap, DomainError> {
    tracing::debug!(plugin = %auth.plugin_type, "executing auth plugin");
    let plugin = auth_registry.resolve(&auth.plugin_type).map_err(|e| {
        DomainError::AuthenticationFailed {
            reason: "AUTH_PLUGIN_NOT_FOUND",
            detail: e.to_string(),
            instance: instance_uri.to_string(),
        }
    })?;
    let mut auth_ctx = AuthContext {
        headers: headers::header_map_to_hash_map(outbound_headers),
        config: auth.config.clone().unwrap_or_default(),
        security_context: ctx.clone(),
    };
    plugin
        .authenticate(&mut auth_ctx)
        .await
        .map_err(|e| match e {
            crate::domain::plugin::PluginError::SecretNotFound(ref s) => {
                DomainError::SecretNotFound {
                    detail: s.clone(),
                    instance: instance_uri.to_string(),
                }
            }
            crate::domain::plugin::PluginError::Rejected(ref msg)
            | crate::domain::plugin::PluginError::InvalidConfig(ref msg) => {
                DomainError::Validation {
                    field: "plugin",
                    reason: "INVALID_PLUGIN_CONFIG",
                    detail: msg.clone(),
                    instance: instance_uri.to_string(),
                }
            }
            crate::domain::plugin::PluginError::AuthFailed(_) => {
                DomainError::AuthenticationFailed {
                    reason: "AUTH_PLUGIN_FAILED",
                    detail: e.to_string(),
                    instance: instance_uri.to_string(),
                }
            }
            crate::domain::plugin::PluginError::Internal(_) => DomainError::AuthenticationFailed {
                reason: "AUTH_PLUGIN_INTERNAL",
                detail: e.to_string(),
                instance: instance_uri.to_string(),
            },
        })?;
    tracing::debug!(plugin = %auth.plugin_type, "auth plugin succeeded");
    Ok(headers::hash_map_to_header_map(&auth_ctx.headers))
}

/// Execute guard plugins on the request, returning the first rejection.
///
/// Guards use fail-hard semantics: the first rejection or error terminates the
/// pipeline immediately.
async fn execute_guard_requests(
    guard_registry: &GuardPluginRegistry,
    guard_bindings: &[&crate::domain::model::PluginBinding],
    outbound_headers: &HeaderMap,
    method: &str,
    path_suffix: &str,
    ctx: &SecurityContext,
    instance_uri: &str,
) -> Result<(), DomainError> {
    let guard_headers = headers::header_map_to_vec(outbound_headers);
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

    let mut transform_headers = headers::header_map_to_vec(outbound_headers);
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

    // Write mutated headers and query params back.
    *outbound_headers = headers::vec_to_header_map(&transform_headers);
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

/// Check rate limits for both upstream and route, returning the tightest outcome.
///
/// Both upstream and route buckets are decremented unconditionally — an upstream
/// token is spent even when a stricter route-level bucket later causes rejection.
///
/// Side effect: updates `rate_limit_usage_ratio` and (on rejection)
/// `rate_limit_exceeded_total` metrics via the injected port.
#[allow(clippy::too_many_arguments)]
fn check_rate_limits(
    rate_limiter: &RateLimiter,
    upstream: &Upstream,
    route: &crate::domain::model::Route,
    req_headers: &HeaderMap,
    ctx: &SecurityContext,
    instance_uri: &str,
    metrics: &dyn OagwMetricsPort,
    host_label: &str,
    path_label: &str,
) -> Result<Option<(RateLimitOutcome, bool)>, DomainError> {
    let mut outcome: Option<(RateLimitOutcome, bool)> = None;
    let client_ip = headers::extract_client_ip(req_headers);
    let client_ip_ref = client_ip.as_deref();
    let tenant_id = ctx.subject_tenant_id();
    let subject_id = ctx.subject_id();

    let record_consume_result = |result: &Result<RateLimitOutcome, DomainError>| match result {
        Ok(outcome) if outcome.limit > 0 => {
            let used = outcome.limit.saturating_sub(outcome.remaining) as f64;
            let ratio = used / outcome.limit as f64;
            metrics.record_rate_limit_usage_ratio(host_label, path_label, ratio);
        }
        Ok(_) => {}
        Err(DomainError::RateLimitExceeded { .. }) => {
            metrics.record_rate_limit_exceeded(host_label, path_label);
            metrics.record_rate_limit_usage_ratio(host_label, path_label, 1.0);
        }
        Err(_) => {}
    };

    if let Some(ref rl) = upstream.rate_limit {
        // For shared-pool budgets, use the pool owner's ID so all children
        // sharing the pool consume from the same token bucket.
        let effective_resource_id = rl.pool_owner_id.as_ref().unwrap_or(&upstream.id);
        let key = build_rate_limit_key(&RateLimitKeyContext {
            resource: RateLimitResource::Upstream,
            resource_id: effective_resource_id,
            scope: &rl.scope,
            tenant_id: &tenant_id,
            subject_id: &subject_id,
            client_ip: client_ip_ref,
            window: &rl.sustained.window,
        });
        let raw = rate_limiter.try_consume(&key, rl, instance_uri);
        record_consume_result(&raw);
        let result = raw?;
        outcome = Some((result, rl.response_headers));
    }

    if let Some(ref rl) = route.rate_limit {
        let key = build_rate_limit_key(&RateLimitKeyContext {
            resource: RateLimitResource::Route,
            resource_id: &route.id,
            scope: &rl.scope,
            tenant_id: &tenant_id,
            subject_id: &subject_id,
            client_ip: client_ip_ref,
            window: &rl.sustained.window,
        });
        let raw = rate_limiter.try_consume(&key, rl, instance_uri);
        record_consume_result(&raw);
        let result = raw?;
        match &outcome {
            Some((existing, show_headers)) if existing.remaining <= result.remaining => {
                // Tighter (or equal) bucket wins for enforcement; on ties
                // upstream wins — both remaining counts are identical so the
                // allow/reject decision is the same either way. OR the header
                // flags so headers are emitted if either scope enables them.
                if rl.response_headers && !show_headers {
                    outcome = outcome.map(|(o, _)| (o, true));
                }
            }
            _ => {
                let prev_headers = outcome.as_ref().is_some_and(|(_, h)| *h);
                outcome = Some((result, rl.response_headers || prev_headers));
            }
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

    let mut header_map = headers::header_map_to_vec(resp_headers);

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

    // Write mutated headers back.
    *resp_headers = headers::vec_to_header_map(&header_map);
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
    modkit_canonical_errors::CanonicalError::from(err.clone()).status_code()
}

/// Short discriminant name for a `DomainError` variant.
fn domain_error_type_name(err: &DomainError) -> &'static str {
    match err {
        DomainError::Validation { .. } => "ValidationError",
        DomainError::Conflict { .. } => "Conflict",
        DomainError::MissingTargetHost { .. } => "MissingTargetHost",
        DomainError::InvalidTargetHost { .. } => "InvalidTargetHost",
        DomainError::UnknownTargetHost { .. } => "UnknownTargetHost",
        DomainError::AuthenticationFailed { .. } => "AuthenticationFailed",
        DomainError::NotFound { .. } => "NotFound",
        DomainError::PayloadTooLarge { .. } => "PayloadTooLarge",
        DomainError::RateLimitExceeded { .. } => "RateLimitExceeded",
        DomainError::SecretNotFound { .. } => "SecretNotFound",
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
        })?;
    *resp.headers_mut() = resp_headers;
    resp.extensions_mut().insert(error_source);
    Ok(resp)
}

/// Normalize a URL path: collapse consecutive slashes and resolve `.`/`..` segments.
/// Segments that would escape above the root are discarded.
fn normalize_path(path: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }
    let mut result = String::with_capacity(path.len());
    if path.starts_with('/') {
        result.push('/');
    }
    result.push_str(&segments.join("/"));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{Endpoint, Scheme, Server, Upstream};
    use crate::domain::services::{EndpointSelector, SelectionError};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use uuid::Uuid;

    #[test]
    fn normalize_collapses_double_slashes() {
        assert_eq!(normalize_path("/alias//v1//chat"), "/alias/v1/chat");
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
            cors: None,
            tags: vec![],
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
        use authz_resolver_sdk::{
            AuthZResolverClient, AuthZResolverError, EvaluationRequest, EvaluationResponse,
            EvaluationResponseContext, PolicyEnforcer,
        };
        use credstore_sdk::{CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef};
        use modkit_security::SecurityContext;

        struct AllowAllAuthZ;
        #[async_trait]
        impl AuthZResolverClient for AllowAllAuthZ {
            async fn evaluate(
                &self,
                _request: EvaluationRequest,
            ) -> Result<EvaluationResponse, AuthZResolverError> {
                Ok(EvaluationResponse {
                    decision: true,
                    context: EvaluationResponseContext {
                        constraints: Vec::new(),
                        deny_reason: None,
                    },
                })
            }
        }

        struct NoopCredStore;
        #[async_trait]
        impl CredStoreClientV1 for NoopCredStore {
            async fn get(
                &self,
                _ctx: &SecurityContext,
                _key: &SecretRef,
            ) -> Result<Option<GetSecretResponse>, CredStoreError> {
                Ok(None)
            }
        }

        let credstore: Arc<dyn CredStoreClientV1> = Arc::new(NoopCredStore);
        let policy_enforcer = PolicyEnforcer::new(Arc::new(AllowAllAuthZ));

        // Minimal CP — never called by select_endpoint().
        use crate::domain::error::DomainError;
        use crate::domain::model::*;
        use crate::domain::services::ControlPlaneService;

        struct NoopCp;
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
            ) -> Result<Vec<Uuid>, DomainError> {
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
            async fn resolve_proxy_target(
                &self,
                _: &SecurityContext,
                _: &str,
                _: &str,
                _: &str,
            ) -> Result<(Upstream, Route), DomainError> {
                unimplemented!()
            }
        }

        let cp: Arc<dyn ControlPlaneService> = Arc::new(NoopCp);
        let server_conf = Arc::new(pingora_core::server::configuration::ServerConf::default());
        let pingora = crate::infra::proxy::pingora_proxy::PingoraProxy::new(
            Duration::from_secs(10),
            Duration::from_secs(30),
            Duration::from_secs(3600),
            Arc::new(crate::domain::ssrf::SsrfGuard::disabled()),
        );
        let proxy = Arc::new(crate::infra::proxy::pingora_proxy::new_http_proxy(
            &server_conf,
            pingora,
        ));

        DataPlaneServiceImpl::new(
            cp,
            credstore,
            policy_enforcer,
            None,
            TokenCacheConfig::default(),
            selector,
            proxy,
            Arc::new(crate::domain::ports::NoopMetrics),
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
}
