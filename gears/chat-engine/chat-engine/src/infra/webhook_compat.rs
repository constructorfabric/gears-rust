//! First-party `webhook-compat` plugin.
//!
//! Adapts legacy HTTP webhook backends to the SDK's [`ChatEngineBackendPlugin`]
//! trait. **This is the only file in the `chat_engine` crate that imports
//! `reqwest`** — keeping every transport concern (auth, retry, timeout,
//! cancellation, error mapping) here is a load-bearing rule from ADR-0022.
//!
//! ## Plugin config schema
//!
//! `PluginCallContext.plugin_config` is opaque JSONB. `webhook-compat` reads
//! the following keys (all under the top-level object):
//!
//! | Key            | Type    | Required | Notes                                                      |
//! |----------------|---------|----------|------------------------------------------------------------|
//! | `endpoint`     | string  | yes      | Base URL (e.g. `https://backend.example/cf-webhook`).      |
//! | `auth`         | object  | no       | One of `{ bearer: "..." }`, `{ api_key_header: "..." }`.   |
//! | `auth_value`   | string  | with auth| Token / API-key value used with the chosen auth strategy.  |
//! | `default_timeout_ms` | u64 | no   | Fallback per-request timeout when `remaining()` is `None`. |
//!
//! Missing required keys surface as `PluginError::invalid_input("missing key: <name>")`.
//! **Values are never logged** — only the key name is included in the
//! diagnostic, per the debug-redaction contract.
//!
//! ## Deadline / cancellation contract
//!
//! Every method that performs HTTP I/O honours the three-state `remaining()`:
//!
//! - `None` → use `default_timeout_ms` if configured, else fall back to
//!   [`DEFAULT_REQUEST_TIMEOUT`]. The per-request timeout is **always set**
//!   so a missing deadline + missing config cannot leave the outbound HTTP
//!   call unbounded.
//! - `Some(Duration::ZERO)` → return [`PluginError::timeout`] **before** any
//!   request is sent. Collapsing this into the `None` branch would let
//!   elapsed deadlines silently extend the budget.
//! - `Some(d > 0)` → forward `d` to the per-request `RequestBuilder::timeout(d)`.
//!
//! The underlying `reqwest::Client` is also built with
//! [`DEFAULT_CONNECT_TIMEOUT`] (TCP/TLS handshake ceiling) and
//! [`DEFAULT_REQUEST_TIMEOUT`] (request-level ceiling). The client-level
//! request timeout is a defence-in-depth fallback for any future code
//! path that issues a request without going through [`build_request`];
//! `RequestBuilder::timeout` overrides it when set.
//!
//! Every HTTP future is raced against `ctx.cancel.cancelled()` via
//! [`tokio::select!`]. The shared cancellation token is **never** invoked by
//! `webhook-compat` (calling `.cancel()` on a clone would propagate back to
//! Chat Engine's parent token); when sub-tasks need independent cancellation,
//! they derive a [`CancellationToken::child_token`].
//!
//! ## Error mapping
//!
//! `reqwest::Error` and HTTP status codes are funnelled through `map_*`
//! helpers to the matching `PluginError` variant — see [`map_status_to_error`]
//! and [`map_reqwest_error`]. The mapping mirrors the table in the SDK docs
//! (do **not** duplicate the `suggested_status` / `is_retryable` matrix —
//! call the SDK helpers if downstream code needs them).
//
// @cpt-cf-chat-engine-webhook-compat:p3

// TODO(phase-15): register WebhookCompatPlugin via ClientHub at module
//                 startup; the registration helper will discover instances
//                 from module config and call
//                 `ctx.client_hub().register_scoped::<dyn ChatEngineBackendPlugin>(
//                     ClientScope::gts_id(&plugin_instance_id), Arc::new(plugin))`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use reqwest::{
    Client, RequestBuilder, Response, StatusCode,
    header::{HeaderMap, HeaderName, HeaderValue, RETRY_AFTER},
};
use serde_json::Value as JsonValue;
use tokio::select;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use chat_engine_sdk::error::PluginError;
use chat_engine_sdk::models::{Capability, HealthStatus, StreamingEvent};
use chat_engine_sdk::plugin::{
    ChatEngineBackendPlugin, MessagePluginCtx, PluginCallContext, PluginStream, SessionPluginCtx,
    SessionPluginResponse, empty_stream, stream_from_events,
};

const CONFIG_KEY_ENDPOINT: &str = "endpoint";
const CONFIG_KEY_AUTH: &str = "auth";
const CONFIG_KEY_AUTH_VALUE: &str = "auth_value";
const CONFIG_KEY_DEFAULT_TIMEOUT_MS: &str = "default_timeout_ms";

/// Hard ceiling on TCP / TLS handshake time for the outbound HTTP client.
/// Without this, a black-holed backend (no SYN-ACK) can wedge a request
/// task indefinitely — reqwest does not enforce a connect timeout by
/// default.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Fallback per-request timeout applied when both `PluginCallContext::
/// remaining()` AND the operator-configured `default_timeout_ms` are
/// absent. Also installed at the `reqwest::Client` level as a
/// defence-in-depth ceiling — `RequestBuilder::timeout` set inside
/// [`build_request`] overrides it on every call we issue today, but the
/// client-level setting protects any future code path that bypasses that
/// helper.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Legacy-HTTP-webhook adapter that implements [`ChatEngineBackendPlugin`].
///
/// One instance per `plugin_instance_id`. Internally holds a `reqwest::Client`
/// (cheap to clone, pools connections) and the GTS instance ID used for
/// ClientHub registration. **All** transport, auth, retry, and timeout logic
/// lives in this struct's methods — Chat Engine core elsewhere holds no HTTP
/// client.
pub struct WebhookCompatPlugin {
    plugin_instance_id: String,
    http: Client,
}

impl WebhookCompatPlugin {
    /// Construct a new instance with a freshly-built `reqwest::Client`. The
    /// client is internally reference-counted; cloning the plugin clones a
    /// handle, not the underlying pool.
    ///
    /// # Errors
    ///
    /// Returns `PluginError::internal` if the underlying `reqwest::Client`
    /// fails to build (e.g., TLS backend init).
    pub fn new(plugin_instance_id: impl Into<String>) -> Result<Self, PluginError> {
        let http = Client::builder()
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .build()
            .map_err(|e| PluginError::internal_with("failed to build reqwest client", e))?;
        Ok(Self {
            plugin_instance_id: plugin_instance_id.into(),
            http,
        })
    }

    /// Construct with a caller-supplied client (for tests / process-wide pool
    /// sharing).
    #[must_use]
    pub fn with_client(plugin_instance_id: impl Into<String>, http: Client) -> Self {
        Self {
            plugin_instance_id: plugin_instance_id.into(),
            http,
        }
    }

    /// Endpoint config record extracted from `PluginCallContext.plugin_config`.
    fn extract_config(call_ctx: &PluginCallContext) -> Result<WebhookConfig, PluginError> {
        let cfg = call_ctx
            .plugin_config
            .as_ref()
            .ok_or_else(|| PluginError::invalid_input("missing key: plugin_config"))?;

        let raw_endpoint = cfg
            .get(CONFIG_KEY_ENDPOINT)
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                PluginError::invalid_input(format!("missing key: {CONFIG_KEY_ENDPOINT}"))
            })?;
        // SSRF guard: reject configured URLs that would aim the outbound
        // POST at loopback / link-local / private / metadata-service IPs
        // before we even build the request. We keep the original string —
        // the canonicalised `Url` value differs in trailing-slash handling
        // and would change the wire payload visible to legacy backends.
        crate::infra::url_guard::validate_outbound_url(raw_endpoint, CONFIG_KEY_ENDPOINT)?;
        let endpoint = raw_endpoint.to_owned();

        let auth_kind = cfg.get(CONFIG_KEY_AUTH).cloned();
        let auth_value = cfg
            .get(CONFIG_KEY_AUTH_VALUE)
            .and_then(JsonValue::as_str)
            .map(str::to_owned);

        let default_timeout = cfg
            .get(CONFIG_KEY_DEFAULT_TIMEOUT_MS)
            .and_then(JsonValue::as_u64)
            .map(Duration::from_millis);

        Ok(WebhookConfig {
            endpoint,
            auth_kind,
            auth_value,
            default_timeout,
        })
    }
}

#[async_trait]
impl ChatEngineBackendPlugin for WebhookCompatPlugin {
    async fn on_session_type_configured(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<SessionPluginResponse, PluginError> {
        let body = serde_json::json!({
            "event": "session_type_configured",
            "session_type_id": ctx.session_type_id,
            "session_id": ctx.session_id,
        });
        let cfg = Self::extract_config(&ctx.call_ctx)?;
        let resp = post_json(
            &self.http,
            &cfg,
            &ctx.call_ctx,
            &body,
            "session_type_configured",
        )
        .await?;
        parse_session_response(resp).await
    }

    async fn on_session_created(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<SessionPluginResponse, PluginError> {
        let body = serde_json::json!({
            "event": "session_created",
            "session_type_id": ctx.session_type_id,
            "session_id": ctx.session_id,
        });
        let cfg = Self::extract_config(&ctx.call_ctx)?;
        let resp = post_json(&self.http, &cfg, &ctx.call_ctx, &body, "session_created").await?;
        parse_session_response(resp).await
    }

    async fn on_session_updated(
        &self,
        ctx: SessionPluginCtx,
    ) -> Result<SessionPluginResponse, PluginError> {
        let body = serde_json::json!({
            "event": "session_updated",
            "session_type_id": ctx.session_type_id,
            "session_id": ctx.session_id,
        });
        let cfg = Self::extract_config(&ctx.call_ctx)?;
        let resp = post_json(&self.http, &cfg, &ctx.call_ctx, &body, "session_updated").await?;
        parse_session_response(resp).await
    }

    async fn on_message(&self, ctx: MessagePluginCtx) -> Result<PluginStream, PluginError> {
        // Detach everything from `&self` before entering the stream body —
        // the returned `PluginStream` is `'static` and Chat Engine drives it
        // long after this `async fn` frame unwinds.
        let cfg = Self::extract_config(&ctx.call_ctx)?;
        let body = serde_json::json!({
            "event": "message",
            "session_id": ctx.session_id,
            "message_id": ctx.message_id,
            "messages": ctx.messages,
        });
        run_streaming_request(
            self.http.clone(),
            cfg,
            ctx.call_ctx.clone(),
            body,
            "message",
        )
        .await
    }

    async fn on_message_recreate(
        &self,
        ctx: MessagePluginCtx,
    ) -> Result<PluginStream, PluginError> {
        let cfg = Self::extract_config(&ctx.call_ctx)?;
        let body = serde_json::json!({
            "event": "message_recreate",
            "session_id": ctx.session_id,
            "message_id": ctx.message_id,
            "messages": ctx.messages,
        });
        run_streaming_request(
            self.http.clone(),
            cfg,
            ctx.call_ctx.clone(),
            body,
            "message_recreate",
        )
        .await
    }

    async fn on_session_summary(&self, ctx: SessionPluginCtx) -> Result<PluginStream, PluginError> {
        let cfg = Self::extract_config(&ctx.call_ctx)?;
        let body = serde_json::json!({
            "event": "session_summary",
            "session_type_id": ctx.session_type_id,
            "session_id": ctx.session_id,
        });
        run_streaming_request(
            self.http.clone(),
            cfg,
            ctx.call_ctx.clone(),
            body,
            "session_summary",
        )
        .await
    }

    async fn health_check(&self) -> Result<HealthStatus, PluginError> {
        // Without per-call config, the plugin has no endpoint to probe.
        // `webhook-compat` is endpoint-per-config, so health surfaces
        // as `Healthy` by default — callers will discover unreachable
        // endpoints via the standard error path on the first real call.
        Ok(HealthStatus::Healthy)
    }

    fn plugin_instance_id(&self) -> &str {
        &self.plugin_instance_id
    }
}

// ---------------------------- internals -----------------------------------

/// Parsed view of the supported `plugin_config` keys.
#[derive(Debug)]
struct WebhookConfig {
    endpoint: String,
    auth_kind: Option<JsonValue>,
    auth_value: Option<String>,
    default_timeout: Option<Duration>,
}

impl WebhookConfig {
    fn resolved_timeout(&self, call_ctx: &PluginCallContext) -> Result<Duration, PluginError> {
        // Three-state remaining() — explicit branches, no collapsing.
        // The `None` branch falls back to the operator-configured
        // `default_timeout_ms` and ultimately to `DEFAULT_REQUEST_TIMEOUT`
        // so the per-request timeout is *always* a positive `Duration` —
        // a missing deadline must never produce an unbounded outbound
        // HTTP call.
        match call_ctx.remaining() {
            Some(d) if d.is_zero() => Err(PluginError::timeout_with(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "deadline elapsed before request",
            ))),
            Some(d) => Ok(d),
            None => Ok(self.default_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT)),
        }
    }

    fn auth_headers(&self) -> Result<HeaderMap, PluginError> {
        let mut headers = HeaderMap::new();
        let Some(kind) = &self.auth_kind else {
            // No auth — explicit, log nothing.
            return Ok(headers);
        };

        // Two supported shapes:
        //   { "auth": { "bearer": true } }            -> uses `auth_value` for Bearer token.
        //   { "auth": { "api_key_header": "X-Key" } } -> uses `auth_value` as the header value.
        if kind.get("bearer").is_some() {
            let value = self.auth_value.as_deref().ok_or_else(|| {
                PluginError::invalid_input(format!("missing key: {CONFIG_KEY_AUTH_VALUE}"))
            })?;
            let header_val = HeaderValue::from_str(&format!("Bearer {value}"))
                .map_err(|_| PluginError::invalid_input("invalid header bytes in auth_value"))?;
            headers.insert(reqwest::header::AUTHORIZATION, header_val);
            return Ok(headers);
        }

        if let Some(name) = kind.get("api_key_header").and_then(JsonValue::as_str) {
            let value = self.auth_value.as_deref().ok_or_else(|| {
                PluginError::invalid_input(format!("missing key: {CONFIG_KEY_AUTH_VALUE}"))
            })?;
            // Preserve the InvalidHeaderName source — the header NAME
            // is operator-supplied config metadata (not a secret), so
            // surfacing why it was rejected aids debugging.
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| PluginError::invalid_input_with("invalid header name in auth", e))?;
            let header_val = HeaderValue::from_str(value)
                .map_err(|_| PluginError::invalid_input("invalid header bytes in auth_value"))?;
            headers.insert(header_name, header_val);
            return Ok(headers);
        }

        Err(PluginError::invalid_input(
            "unsupported auth kind (expected `bearer` or `api_key_header`)",
        ))
    }
}

/// Build a `POST {endpoint}` request with auth + a per-request timeout
/// applied. The timeout is non-optional — see [`WebhookConfig::
/// resolved_timeout`] for the fallback chain.
fn build_request(
    http: &Client,
    cfg: &WebhookConfig,
    call_ctx: &PluginCallContext,
    body: &JsonValue,
) -> Result<RequestBuilder, PluginError> {
    let timeout = cfg.resolved_timeout(call_ctx)?;
    let req = http
        .post(&cfg.endpoint)
        .headers(cfg.auth_headers()?)
        .json(body)
        .timeout(timeout);
    Ok(req)
}

/// POST and return the unbuffered `Response`, racing the HTTP future against
/// the caller's `CancellationToken`. Performs the pre-flight cancellation
/// check before issuing any request.
async fn post_json(
    http: &Client,
    cfg: &WebhookConfig,
    call_ctx: &PluginCallContext,
    body: &JsonValue,
    method_label: &'static str,
) -> Result<Response, PluginError> {
    // Pre-flight: already cancelled?
    if call_ctx.is_cancelled() {
        return Err(PluginError::transient("cancelled"));
    }

    let started = Instant::now();
    let req = build_request(http, cfg, call_ctx, body)?;
    let cancel = call_ctx.cancel.clone();
    let request_id = call_ctx.request_id;

    let resp = select! {
        biased;
        _ = cancel.cancelled() => {
            // If cancellation was deadline-driven, Some(ZERO) is the more
            // accurate signal — but at this point we don't know which. The
            // safer bet is `transient("cancelled")`; deadline-elapsed callers
            // would have tripped the ZERO branch in `resolved_timeout`.
            return Err(PluginError::transient("cancelled"));
        }
        r = req.send() => r.map_err(map_reqwest_error)?,
    };

    let status = resp.status();
    if !status.is_success() {
        // Pull the Retry-After header before consuming the response body.
        let retry_after = parse_retry_after(resp.headers());
        let elapsed_ms = started.elapsed().as_millis() as u64;
        warn!(
            request_id = %request_id,
            method = method_label,
            status = status.as_u16(),
            elapsed_ms,
            "webhook-compat received non-success status"
        );
        return Err(map_status_to_error(status, retry_after));
    }

    Ok(resp)
}

/// Build the streaming `PluginStream` for `on_message` / `on_message_recreate`
/// / `on_session_summary`. The stream owns clones of the `Client`, the
/// `PluginCallContext`, and a `CancellationToken` so it satisfies `'static`.
async fn run_streaming_request(
    http: Client,
    cfg: WebhookConfig,
    call_ctx: PluginCallContext,
    body: JsonValue,
    method_label: &'static str,
) -> Result<PluginStream, PluginError> {
    let cancel = call_ctx.cancel.clone();
    let request_id = call_ctx.request_id;

    // Pre-flight cancellation.
    if call_ctx.is_cancelled() {
        return Err(PluginError::transient("cancelled"));
    }

    // Three-state deadline check happens inside `build_request` →
    // `resolved_timeout`. Issue the request now so we can fail-early before
    // returning a stream.
    let req = build_request(&http, &cfg, &call_ctx, &body)?;
    let started = Instant::now();

    let resp = select! {
        biased;
        _ = cancel.cancelled() => {
            return Err(PluginError::transient("cancelled"));
        }
        r = req.send() => r.map_err(map_reqwest_error)?,
    };

    let status = resp.status();
    if !status.is_success() {
        let retry_after = parse_retry_after(resp.headers());
        let elapsed_ms = started.elapsed().as_millis() as u64;
        warn!(
            request_id = %request_id,
            method = method_label,
            status = status.as_u16(),
            elapsed_ms,
            "webhook-compat streaming setup received non-success status"
        );
        return Err(map_status_to_error(status, retry_after));
    }

    // Body stream of NDJSON-encoded `StreamingEvent`s, line-delimited.
    let byte_stream = resp.bytes_stream();
    Ok(Box::pin(ndjson_event_stream(byte_stream, cancel)))
}

/// Adapt a byte stream into a `PluginStream` of NDJSON-encoded
/// [`StreamingEvent`]s. Mid-stream items may surface as `Err(PluginError)`;
/// per the SDK contract this does NOT end the outer `Result`.
fn ndjson_event_stream<S>(
    bytes: S,
    cancel: CancellationToken,
) -> impl Stream<Item = Result<StreamingEvent, PluginError>> + Send + 'static
where
    S: Stream<Item = reqwest::Result<bytes::Bytes>> + Send + 'static,
{
    use futures::stream::unfold;

    struct State<S> {
        inner: S,
        buf: Vec<u8>,
        cancel: CancellationToken,
        done: bool,
    }

    let state = State {
        inner: Box::pin(bytes),
        buf: Vec::with_capacity(1024),
        cancel,
        done: false,
    };

    unfold(state, |mut s| async move {
        if s.done {
            return None;
        }
        loop {
            // Drain complete lines from the buffer.
            if let Some(pos) = s.buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = s.buf.drain(..=pos).collect();
                let trimmed = trim_newline(&line);
                if trimmed.is_empty() {
                    continue;
                }
                match serde_json::from_slice::<StreamingEvent>(trimmed) {
                    Ok(evt) => return Some((Ok(evt), s)),
                    Err(e) => {
                        s.done = true;
                        return Some((
                            Err(PluginError::internal_with("malformed NDJSON event", e)),
                            s,
                        ));
                    }
                }
            }

            // No complete line yet — wait for more bytes, racing against
            // cancellation.
            let next = select! {
                biased;
                _ = s.cancel.cancelled() => {
                    s.done = true;
                    return Some((Err(PluginError::transient("cancelled")), s));
                }
                r = s.inner.next() => r,
            };

            match next {
                Some(Ok(chunk)) => {
                    s.buf.extend_from_slice(&chunk);
                }
                Some(Err(e)) => {
                    s.done = true;
                    return Some((Err(map_reqwest_error(e)), s));
                }
                None => {
                    s.done = true;
                    // Flush any trailing event (no terminal `\n`).
                    let trimmed = trim_newline(&s.buf);
                    if trimmed.is_empty() {
                        return None;
                    }
                    match serde_json::from_slice::<StreamingEvent>(trimmed) {
                        Ok(evt) => {
                            s.buf.clear();
                            return Some((Ok(evt), s));
                        }
                        Err(e) => {
                            return Some((
                                Err(PluginError::internal_with("malformed NDJSON event", e)),
                                s,
                            ));
                        }
                    }
                }
            }
        }
    })
}

fn trim_newline(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

/// Parse a session-hook HTTP response into a [`SessionPluginResponse`].
///
/// Accepts two shapes for backward compatibility:
/// - a bare capability array `[ {..}, .. ]` → capabilities only, no metadata;
/// - an object `{ "capabilities": [..], "metadata": {..} }` → both, where
///   `metadata` is forwarded verbatim to be merged into the session.
async fn parse_session_response(resp: Response) -> Result<SessionPluginResponse, PluginError> {
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| PluginError::transient_with("failed to read response body", e))?;
    if bytes.is_empty() {
        return Ok(SessionPluginResponse::default());
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|e| PluginError::internal_with("malformed session response", e))?;
    match value {
        serde_json::Value::Array(_) => {
            let capabilities = serde_json::from_value::<Vec<Capability>>(value)
                .map_err(|e| PluginError::internal_with("malformed capability payload", e))?;
            Ok(SessionPluginResponse::from(capabilities))
        }
        serde_json::Value::Object(mut map) => {
            let capabilities = match map.remove("capabilities") {
                Some(c) => serde_json::from_value::<Vec<Capability>>(c)
                    .map_err(|e| PluginError::internal_with("malformed capability payload", e))?,
                None => Vec::new(),
            };
            let metadata = map.remove("metadata").filter(|v| !v.is_null());
            Ok(SessionPluginResponse {
                capabilities,
                metadata,
            })
        }
        _ => Err(PluginError::internal(
            "session response must be a capability array or an object",
        )),
    }
}

/// Map an HTTP status (4xx/5xx) to a [`PluginError`] using the canonical
/// table from the SDK. `retry_after` is forwarded for `429` responses.
fn map_status_to_error(status: StatusCode, retry_after: Option<Duration>) -> PluginError {
    match status.as_u16() {
        429 => PluginError::rate_limited(retry_after),
        401 | 403 => PluginError::unauthorized(format!("upstream returned {status}")),
        404 => PluginError::not_found(format!("upstream returned {status}")),
        400 | 422 => PluginError::invalid_input(format!("upstream returned {status}")),
        s if (500..=599).contains(&s) => {
            PluginError::transient(format!("upstream returned {status}"))
        }
        _ => PluginError::internal(format!("unexpected upstream status {status}")),
    }
}

fn map_reqwest_error(err: reqwest::Error) -> PluginError {
    if err.is_timeout() {
        return PluginError::timeout_with(err);
    }
    if err.is_connect() || err.is_request() {
        return PluginError::transient_with("network error", err);
    }
    if let Some(status) = err.status() {
        // status() can also expose 5xx after explicit error_for_status().
        return map_status_to_error(status, None);
    }
    PluginError::internal_with("reqwest error", err)
}

fn parse_retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
}

// ---------------------------------------------------------------------------

/// Internal: produce an empty/no-op `PluginStream` and an explicit
/// `Capability` echo from a captured `Arc<Self>`. This helper is currently
/// unused but documents the canonical pattern for plugin authors that need
/// `Arc<Self>` to stay alive inside an `async_stream::stream!` body.
///
/// Kept here so that future contributors don't accidentally re-introduce a
/// borrow of `&self` inside a `'static` stream. Marked `#[allow(dead_code)]`
/// until a future phase wires the demo pathway.
#[allow(dead_code)]
fn _doc_arc_self_pattern(me: Arc<WebhookCompatPlugin>) -> PluginStream {
    // Even though `me` is captured, no stream output references `&self.x`.
    let _id = me.plugin_instance_id().to_owned();
    stream_from_events(Vec::new())
}

#[allow(dead_code)]
fn _doc_empty() -> PluginStream {
    empty_stream()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use chat_engine_sdk::models::{TenantId, UserId};

    fn make_call_ctx(cfg: Option<JsonValue>, cancel: CancellationToken) -> PluginCallContext {
        PluginCallContext {
            request_id: Uuid::nil(),
            tenant_id: TenantId::new("t"),
            user_id: UserId::new("u"),
            plugin_instance_id: "p".into(),
            session_type_id: Uuid::nil(),
            plugin_config: cfg,
            enabled_capabilities: None,
            deadline: None,
            cancel,
        }
    }

    #[test]
    fn extract_config_rejects_missing_endpoint() {
        let ctx = make_call_ctx(Some(serde_json::json!({})), CancellationToken::new());
        let err = WebhookCompatPlugin::extract_config(&ctx).unwrap_err();
        // We only assert on the *kind*; the message names the key.
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn extract_config_rejects_absent_config() {
        let ctx = make_call_ctx(None, CancellationToken::new());
        let err = WebhookCompatPlugin::extract_config(&ctx).unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn extract_config_parses_full_object() {
        let cfg = serde_json::json!({
            "endpoint": "https://example.invalid/webhook",
            "auth": { "bearer": true },
            "auth_value": "tok",
            "default_timeout_ms": 1500_u64,
        });
        let ctx = make_call_ctx(Some(cfg), CancellationToken::new());
        let parsed = WebhookCompatPlugin::extract_config(&ctx).expect("ok");
        assert_eq!(parsed.endpoint, "https://example.invalid/webhook");
        assert_eq!(parsed.default_timeout, Some(Duration::from_millis(1500)));
        assert_eq!(parsed.auth_value.as_deref(), Some("tok"));
    }

    #[test]
    fn resolved_timeout_some_zero_returns_timeout() {
        let cfg = WebhookConfig {
            endpoint: "x".into(),
            auth_kind: None,
            auth_value: None,
            default_timeout: None,
        };
        let ctx = PluginCallContext {
            request_id: Uuid::nil(),
            tenant_id: TenantId::new("t"),
            user_id: UserId::new("u"),
            plugin_instance_id: "p".into(),
            session_type_id: Uuid::nil(),
            plugin_config: None,
            enabled_capabilities: None,
            deadline: Some(Instant::now().checked_sub(Duration::from_secs(1)).unwrap()),
            cancel: CancellationToken::new(),
        };
        let err = cfg.resolved_timeout(&ctx).unwrap_err();
        assert!(matches!(err, PluginError::Timeout { .. }));
    }

    #[test]
    fn resolved_timeout_none_returns_default() {
        let cfg = WebhookConfig {
            endpoint: "x".into(),
            auth_kind: None,
            auth_value: None,
            default_timeout: Some(Duration::from_millis(750)),
        };
        let ctx = make_call_ctx(None, CancellationToken::new());
        let out = cfg.resolved_timeout(&ctx).expect("ok");
        assert_eq!(out, Duration::from_millis(750));
    }

    #[test]
    fn resolved_timeout_none_and_no_config_falls_back_to_hard_ceiling() {
        // Pre-fix this combination returned `Ok(None)` → no per-request
        // timeout → a hung backend could wedge the outbound HTTP task
        // forever. The fix collapses the fallback chain into a positive
        // `Duration`, so the per-request timeout is always set.
        let cfg = WebhookConfig {
            endpoint: "x".into(),
            auth_kind: None,
            auth_value: None,
            default_timeout: None,
        };
        let ctx = make_call_ctx(None, CancellationToken::new());
        let out = cfg.resolved_timeout(&ctx).expect("ok");
        assert_eq!(
            out, DEFAULT_REQUEST_TIMEOUT,
            "missing deadline + missing config must fall back to the hard ceiling, \
             never to an unbounded request",
        );
    }

    #[test]
    fn resolved_timeout_positive_forwards() {
        let cfg = WebhookConfig {
            endpoint: "x".into(),
            auth_kind: None,
            auth_value: None,
            default_timeout: None,
        };
        let mut ctx = make_call_ctx(None, CancellationToken::new());
        ctx.deadline = Some(Instant::now() + Duration::from_secs(5));
        let out = cfg.resolved_timeout(&ctx).expect("ok");
        assert!(out > Duration::from_secs(4) && out <= Duration::from_secs(5));
    }

    #[test]
    fn auth_bearer_sets_authorization_header() {
        let cfg = WebhookConfig {
            endpoint: "x".into(),
            auth_kind: Some(serde_json::json!({ "bearer": true })),
            auth_value: Some("abc".into()),
            default_timeout: None,
        };
        let h = cfg.auth_headers().expect("ok");
        assert_eq!(
            h.get(reqwest::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "Bearer abc"
        );
    }

    #[test]
    fn auth_api_key_header_sets_custom_header() {
        let cfg = WebhookConfig {
            endpoint: "x".into(),
            auth_kind: Some(serde_json::json!({ "api_key_header": "X-Plugin-Key" })),
            auth_value: Some("secret".into()),
            default_timeout: None,
        };
        let h = cfg.auth_headers().expect("ok");
        assert_eq!(h.get("X-Plugin-Key").unwrap().to_str().unwrap(), "secret");
    }

    #[test]
    fn auth_missing_value_errors() {
        let cfg = WebhookConfig {
            endpoint: "x".into(),
            auth_kind: Some(serde_json::json!({ "bearer": true })),
            auth_value: None,
            default_timeout: None,
        };
        let err = cfg.auth_headers().unwrap_err();
        assert!(matches!(err, PluginError::InvalidInput { .. }));
    }

    #[test]
    fn status_mapping_table() {
        assert!(matches!(
            map_status_to_error(StatusCode::from_u16(401).unwrap(), None),
            PluginError::Unauthorized { .. }
        ));
        assert!(matches!(
            map_status_to_error(StatusCode::from_u16(404).unwrap(), None),
            PluginError::NotFound { .. }
        ));
        assert!(matches!(
            map_status_to_error(
                StatusCode::from_u16(429).unwrap(),
                Some(Duration::from_secs(7))
            ),
            PluginError::RateLimited { .. }
        ));
        assert!(matches!(
            map_status_to_error(StatusCode::from_u16(503).unwrap(), None),
            PluginError::Transient { .. }
        ));
        assert!(matches!(
            map_status_to_error(StatusCode::from_u16(400).unwrap(), None),
            PluginError::InvalidInput { .. }
        ));
    }

    #[test]
    fn plugin_instance_id_returns_constructor_value() {
        let p = WebhookCompatPlugin::with_client("gts.cf.webhook.v1~vendor", Client::new());
        assert_eq!(p.plugin_instance_id(), "gts.cf.webhook.v1~vendor");
    }

    #[tokio::test]
    async fn pre_flight_cancellation_short_circuits() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let cfg = WebhookConfig {
            endpoint: "http://127.0.0.1:1".into(),
            auth_kind: None,
            auth_value: None,
            default_timeout: None,
        };
        let ctx = make_call_ctx(None, cancel);
        let err = post_json(&Client::new(), &cfg, &ctx, &serde_json::json!({}), "test")
            .await
            .unwrap_err();
        assert!(matches!(err, PluginError::Transient { .. }));
    }
}
