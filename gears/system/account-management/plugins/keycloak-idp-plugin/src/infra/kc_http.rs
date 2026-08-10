//! Reqwest-backed implementation of [`KcTransport`].
//!
//! All reqwest imports are confined to this file. Domain code interacts only
//! with the [`crate::domain::kc::transport::KcTransport`] trait.

use opentelemetry::propagation::{Injector, TextMapPropagator};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use reqwest::{Client, Method, RequestBuilder};
use serde_json::Value;
use std::sync::OnceLock;
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::domain::error::{KcStatusKind, PluginError, redact_secrets};
use crate::domain::kc::transport::{KcTransport, truncate_2kb};

/// Truncate + redact the `Display` form of an error before stuffing it into
/// `PluginError::KcRest::body_first_2kb`.
///
/// DESIGN §4.4 mandates that every `KcRest` construction site on the transport
/// seam goes through `truncate_2kb` + `redact_secrets`. Without this helper the
/// transport-error / build-error paths embedded raw `reqwest::Error` strings,
/// which can carry URLs (operationally sensitive) or, on the token-endpoint
/// `form_post` path, fragments of the form-encoded `client_secret`.
fn redact_error_body(s: &str) -> String {
    redact_secrets(&truncate_2kb(s))
}

/// Adapts a `reqwest::HeaderMap` to the `OTel` `Injector` interface so that the
/// W3C `TraceContextPropagator` can write `traceparent` / `tracestate` headers
/// directly into an outgoing request.
struct HeaderInjector<'a>(&'a mut reqwest::header::HeaderMap);

impl Injector for HeaderInjector<'_> {
    fn set(&mut self, key: &str, value: String) {
        if let (Ok(name), Ok(val)) = (
            reqwest::header::HeaderName::from_bytes(key.as_bytes()),
            reqwest::header::HeaderValue::from_str(&value),
        ) {
            self.0.insert(name, val);
        }
    }
}

/// Returns the process-wide W3C `TraceContextPropagator` instance (lazy-init).
///
/// Initialisation is deliberately local — the global `OTel` propagator is an
/// application-level concern (`vpserver` main) and is intentionally *not* set
/// here. Using a private `OnceLock` keeps this crate self-contained and
/// testable without touching global state.
fn propagator() -> &'static TraceContextPropagator {
    static P: OnceLock<TraceContextPropagator> = OnceLock::new();
    P.get_or_init(TraceContextPropagator::new)
}

/// Inject W3C `traceparent` (and `tracestate` when present) from the current
/// tracing span into the request's header map. Per DESIGN §8.2 this binds
/// every Keycloak Admin REST hop to the platform trace tree.
fn inject_trace_headers(headers: &mut reqwest::header::HeaderMap) {
    let context = tracing::Span::current().context();
    propagator().inject_context(&context, &mut HeaderInjector(headers));
}

/// Map `reqwest::Method` to the static string the `PluginError::KcRest.method`
/// field expects. We keep `&'static str` (not `String`) on the error because
/// HTTP verbs are a fixed set and this stays grep-friendly.
///
/// Narrowed to the canonical verb set the plugin actually issues against
/// Keycloak Admin REST: GET / POST / PUT / DELETE. PATCH / HEAD / OPTIONS
/// used to be paranoia branches with no callers — dropped to keep the
/// verb whitelist single-sourced with `raw_request`'s match.
///
/// `http::Method` is non-Copy (it stores an inline tag for standard
/// methods and a `Box<[u8]>` for extension methods), so we take it by
/// shared reference rather than by value.
#[must_use]
pub(crate) fn method_static(m: &Method) -> &'static str {
    match *m {
        Method::GET => "GET",
        Method::POST => "POST",
        Method::PUT => "PUT",
        Method::DELETE => "DELETE",
        _ => "OTHER",
    }
}

/// Whether a verb is safe to replay after a connect/timeout error — i.e.
/// re-sending it cannot duplicate a side effect. GET/PUT/DELETE are
/// idempotent; POST is not (re-POSTing a create can duplicate it, as the
/// duplicate `POST /admin/realms` in run am-functional-22 showed). The
/// token-endpoint `form_post` is also a POST but opts back into retry
/// explicitly because re-fetching a token is harmless.
fn idempotent_method(m: &Method) -> bool {
    matches!(*m, Method::GET | Method::PUT | Method::DELETE)
}

/// Construct a `PluginError::KcRest` from a transport error (no HTTP exchange).
#[must_use]
pub(crate) fn transport_error(
    method: &Method,
    path_template: &str,
    e: &reqwest::Error,
) -> PluginError {
    let status = if e.is_timeout() {
        KcStatusKind::Timeout
    } else {
        KcStatusKind::Transport
    };
    PluginError::KcRest {
        method: method_static(method),
        path_template: path_template.into(),
        status,
        body_first_2kb: redact_error_body(&e.to_string()),
    }
}

/// Construct a `PluginError::KcRest` from a non-success HTTP response.
#[must_use]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "used only from kc_http_tests.rs; the companion-file test cannot import it otherwise"
    )
)]
pub(crate) fn http_status_error(
    method: &Method,
    path_template: &str,
    status: u16,
    body: &str,
) -> PluginError {
    PluginError::KcRest {
        method: method_static(method),
        path_template: path_template.into(),
        status: KcStatusKind::Http(status),
        body_first_2kb: redact_error_body(body),
    }
}

/// Reqwest-backed implementation of [`KcTransport`].
///
/// Constructed once per process (typically in `module.rs`) from a
/// pre-configured `reqwest::Client`. The TLS bundle, timeout, and other
/// transport-level settings are baked into the client at construction time.
///
/// Fields are `pub(crate)` so the cfg(test) companion `kc_http_tests`
/// can attach test-only constructors without splitting tests across two
/// files (DE1101).
pub struct ReqwestKcTransport {
    pub(crate) http: Client,
    pub(crate) retry_policy: crate::config::HttpRetryPolicy,
}

impl ReqwestKcTransport {
    /// Build a `ReqwestKcTransport` from the plugin's Keycloak config.
    ///
    /// When `cfg.tls_ca_bundle_ref` is `Some(ref_id)` the referenced PEM
    /// bundle is fetched from `CredStore` and installed as an additional trust
    /// anchor (DESIGN §4.4 `tls_ca_bundle_ref`). When `None`, the system trust
    /// store is used.
    ///
    /// # Errors
    /// * [`crate::domain::error::PluginError::CredStoreRead`] when
    ///   `tls_ca_bundle_ref` is set but `CredStore` returns an error or `None`.
    /// * [`crate::domain::error::PluginError::Config`] when the PEM cannot be
    ///   parsed or the `reqwest::Client` builder fails.
    pub async fn from_config(
        cfg: &crate::config::KeycloakConfig,
        cs_reader: &crate::domain::credstore::CredStoreReader,
    ) -> Result<Self, crate::domain::error::PluginError> {
        use secrecy::ExposeSecret as _;
        use std::time::Duration;

        let mut builder =
            Client::builder().timeout(Duration::from_millis(cfg.http_request_timeout_ms));
        if let Some(ref_id) = cfg.tls_ca_bundle_ref.as_deref() {
            let secret_ref = credstore_sdk::SecretRef::new(ref_id).map_err(|e| {
                crate::domain::error::PluginError::CredStoreRead {
                    detail: format!("invalid tls_ca_bundle_ref '{ref_id}': {e}"),
                }
            })?;
            let pem = cs_reader.get(&secret_ref).await.map_err(|e| {
                crate::domain::error::PluginError::CredStoreRead {
                    detail: format!("read tls_ca_bundle_ref '{ref_id}': {e}"),
                }
            })?;
            let pem = pem.ok_or_else(|| crate::domain::error::PluginError::CredStoreRead {
                detail: format!("tls_ca_bundle_ref '{ref_id}' not found"),
            })?;
            let cert =
                reqwest::Certificate::from_pem(pem.expose_secret().as_bytes()).map_err(|e| {
                    crate::domain::error::PluginError::Config {
                        detail: format!("tls_ca_bundle_ref '{ref_id}' is not a valid PEM: {e}"),
                    }
                })?;
            builder = builder.add_root_certificate(cert);
        }
        let http = builder
            .build()
            .map_err(|e| crate::domain::error::PluginError::Config {
                detail: format!("reqwest client build failed: {e}"),
            })?;
        Ok(Self {
            http,
            retry_policy: cfg.http_retry_policy.clone(),
        })
    }

    /// Compute backoff delay for the given attempt index (0-based: attempt 0 = first retry).
    /// Returns duration in milliseconds.
    fn backoff_ms(&self, attempt: u32) -> u64 {
        use crate::config::JitterKind;
        let policy = &self.retry_policy;
        // Exponential: initial * 2^attempt, capped at max_backoff_ms.
        let shift = attempt.min(10);
        let multiplier = 1u64.checked_shl(shift).unwrap_or(1);
        let base = policy.initial_backoff_ms.saturating_mul(multiplier);
        let capped = base.min(policy.max_backoff_ms);
        match policy.jitter {
            JitterKind::Full => {
                use rand::RngExt as _;
                rand::rng().random_range(0..=capped)
            }
            JitterKind::None => capped,
        }
    }

    /// Parse the `Retry-After` header value into a backoff in milliseconds,
    /// CAPPED by `retry_policy.max_backoff_ms` so a misbehaving server
    /// can't pin a request thread for an unbounded delay.
    ///
    /// Only the delta-seconds form of [`RFC 9110 §10.2.3`] is honoured
    /// (`Retry-After: 30`). The HTTP-date form (`Retry-After: Fri, 31
    /// Dec 1999 23:59:59 GMT`) is rare in practice for KC's 429/503
    /// surface and is intentionally ignored — fall back to the
    /// computed exponential backoff in that case.
    ///
    /// [`RFC 9110 §10.2.3`]: https://www.rfc-editor.org/rfc/rfc9110#name-retry-after
    fn parse_retry_after_ms(&self, headers: &reqwest::header::HeaderMap) -> Option<u64> {
        let value = headers.get(reqwest::header::RETRY_AFTER)?;
        let secs: u64 = value.to_str().ok()?.trim().parse().ok()?;
        let ms = secs.saturating_mul(1000);
        Some(ms.min(self.retry_policy.max_backoff_ms))
    }

    /// Execute a request-builder factory with retry logic.
    ///
    /// `build` is called once per attempt to produce a fresh `reqwest::Request`
    /// (requests are consumed on `execute`). Returns `Err` immediately if
    /// `build()` itself fails.
    ///
    /// `retry_transport_errors` gates retrying connect/timeout failures
    /// (VHP-190): a transport error does NOT prove the server skipped the
    /// request, so replaying a NON-idempotent verb can duplicate a side
    /// effect (the double `POST /admin/realms` → duplicate realm → false
    /// ambiguous 500). Callers pass `false` for non-idempotent POSTs and
    /// `true` for idempotent verbs (GET/PUT/DELETE) and the safe-to-replay
    /// token `form_post`. Retryable HTTP statuses (5xx/429) are unaffected.
    async fn execute_with_retry<F>(
        &self,
        build: F,
        method: &Method,
        path_template: &str,
        retry_transport_errors: bool,
    ) -> Result<(u16, String), crate::domain::error::PluginError>
    where
        F: Fn() -> Result<reqwest::Request, crate::domain::error::PluginError>,
    {
        let policy = &self.retry_policy;
        let mut attempt: u32 = 0;
        loop {
            let req = build()?;
            let result = self.http.execute(req).await;
            // Classify transport errors before inspecting the result.
            // `is_connect()` / `is_timeout()` are the genuine transient
            // signals. `is_request()` covers errors that originate in
            // request CONSTRUCTION (invalid URI escape, header build,
            // body-encoding failures) — none transient; retrying just
            // burns the entire `max_retries * max_backoff_ms` budget on
            // the same construction failure. Drop it from the predicate.
            let retryable_transport = retry_transport_errors
                && matches!(&result, Err(e) if e.is_connect() || e.is_timeout());
            match result {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if crate::domain::error::is_retryable_status(status)
                        && attempt < policy.max_retries
                    {
                        // Honour `Retry-After` (RFC 9110 §10.2.3) before
                        // falling back to the exponential schedule.
                        // Reading the header BEFORE consuming the body
                        // is fine — `reqwest::Response::headers` is
                        // available pre-body. Header is capped at
                        // `max_backoff_ms` inside `parse_retry_after_ms`
                        // so a hostile server can't pin the worker.
                        let retry_after = self.parse_retry_after_ms(resp.headers());
                        // Consume the body so the connection can be reused.
                        let _ = resp.bytes().await;
                        let delay = retry_after.unwrap_or_else(|| self.backoff_ms(attempt));
                        tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                        attempt += 1;
                        // continue to next attempt
                    } else {
                        // Preserve the observed HTTP status when the body
                        // read fails — `with_reactive_401` / per-realm
                        // error classifiers branch on `KcStatusKind::Http(_)`,
                        // and demoting a real reply to `Transport` on a
                        // body-read blip would misroute 401 / 5xx retry
                        // semantics. Synthesise a body-first-2kb that
                        // tells the operator what happened.
                        match resp.bytes().await {
                            Ok(body_bytes) => {
                                let body = String::from_utf8_lossy(&body_bytes).into_owned();
                                return Ok((status, body));
                            }
                            Err(e) => {
                                return Err(crate::domain::error::PluginError::KcRest {
                                    method: method_static(method),
                                    path_template: path_template.to_owned(),
                                    status: crate::domain::error::KcStatusKind::Http(status),
                                    body_first_2kb: redact_error_body(&format!(
                                        "<body-read-failed: {e}>"
                                    )),
                                });
                            }
                        }
                    }
                }
                Err(ref e) if retryable_transport && attempt < policy.max_retries => {
                    let delay = self.backoff_ms(attempt);
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    attempt += 1;
                }
                Err(e) => {
                    return Err(transport_error(method, path_template, &e));
                }
            }
        }
    }
}

#[async_trait::async_trait]
impl KcTransport for ReqwestKcTransport {
    async fn raw_request(
        &self,
        method: &str,
        url: &str,
        bearer_token: &str,
        body: Option<&Value>,
        path_template: &str,
    ) -> Result<(u16, String), PluginError> {
        // Restrict to the verb set the plugin actually issues against
        // Keycloak Admin REST: GET / POST / PUT / DELETE. The previous
        // `parse().unwrap_or(Method::GET)` was doubly wrong:
        // `http::Method::from_str` accepts arbitrary extension methods
        // (e.g. `"DLETE"` parses fine and would have been sent over the
        // wire), and the GET fallback only kicked in for truly-invalid
        // bytes (empty / whitespace / non-ASCII). PATCH/HEAD/OPTIONS were
        // also in this whitelist as paranoia placeholders with no
        // callers — pruning them keeps the verb set single-sourced with
        // `method_static`. Reject anything outside the canonical set so
        // a typo in a path-template constant surfaces as
        // `PluginError::Config` instead of either a silent GET or a
        // server 405.
        let m = match method {
            "GET" => Method::GET,
            "POST" => Method::POST,
            "PUT" => Method::PUT,
            "DELETE" => Method::DELETE,
            _ => {
                return Err(PluginError::Config {
                    detail: format!("invalid HTTP method in raw_request: {method}"),
                });
            }
        };
        // Clone the inputs we need inside the closure. The `Fn` bound on
        // `execute_with_retry`'s `build` requires the closure to produce
        // a fresh `Request` on every invocation (retry path constructs
        // the request again). `http::Method` is non-Copy, so the closure
        // captures an owned `m_clone` and clones it per invocation.
        let url = url.to_owned();
        let bearer_token = bearer_token.to_owned();
        let body = body.cloned();
        let http = self.http.clone();
        let m_clone = m.clone();
        let pt = path_template.to_owned();
        let build = move || {
            let mut req: RequestBuilder = http
                .request(m_clone.clone(), &url)
                .bearer_auth(&bearer_token);
            if let Some(ref b) = body {
                req = req.json(b);
            }
            let mut built = req
                .build()
                .map_err(|e| crate::domain::error::PluginError::KcRest {
                    method: method_static(&m_clone),
                    path_template: pt.clone(),
                    status: crate::domain::error::KcStatusKind::Transport,
                    body_first_2kb: redact_error_body(&e.to_string()),
                })?;
            inject_trace_headers(built.headers_mut());
            Ok(built)
        };
        // Non-idempotent verbs (POST) must NOT be replayed on a connect/
        // timeout error — the request may already have taken effect server
        // side (VHP-190). Idempotent verbs are safe to retry.
        let retry_transport = idempotent_method(&m);
        self.execute_with_retry(build, &m, path_template, retry_transport)
            .await
    }

    async fn form_post(
        &self,
        url: &str,
        fields: &[(&str, &str)],
        path_template: &str,
    ) -> Result<(u16, String), PluginError> {
        let url = url.to_owned();
        let fields_owned: Vec<(String, String)> = fields
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        let http = self.http.clone();
        let pt = path_template.to_owned();
        let build = move || {
            let refs: Vec<(&str, &str)> = fields_owned
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let mut built = http.post(&url).form(&refs).build().map_err(|e| {
                crate::domain::error::PluginError::KcRest {
                    method: "POST",
                    path_template: pt.clone(),
                    status: crate::domain::error::KcStatusKind::Transport,
                    body_first_2kb: redact_error_body(&e.to_string()),
                }
            })?;
            // DESIGN §8.2: every KC Admin REST hop carries W3C
            // traceparent from the current span. Token-endpoint POSTs
            // are admin hops too — without this, cold-cache token
            // failures lose the platform trace link.
            inject_trace_headers(built.headers_mut());
            Ok(built)
        };
        // A token fetch is a POST, but re-fetching a token has no durable
        // side effect, so it opts back into transport retry — important
        // under load where the token endpoint is the bottleneck (VHP-190).
        self.execute_with_retry(build, &Method::POST, path_template, true)
            .await
    }

    async fn get_unauthenticated(
        &self,
        url: &str,
        path_template: &str,
    ) -> Result<(u16, String), PluginError> {
        let url = url.to_owned();
        let http = self.http.clone();
        let pt = path_template.to_owned();
        let build = move || {
            let mut built =
                http.get(&url)
                    .build()
                    .map_err(|e| crate::domain::error::PluginError::KcRest {
                        method: "GET",
                        path_template: pt.clone(),
                        status: crate::domain::error::KcStatusKind::Transport,
                        body_first_2kb: redact_error_body(&e.to_string()),
                    })?;
            // DESIGN §8.2: health-probe GETs reach KC and MUST carry
            // traceparent so a flat outage surfaces with the platform
            // trace link intact for post-mortem.
            inject_trace_headers(built.headers_mut());
            Ok(built)
        };
        self.execute_with_retry(build, &Method::GET, path_template, true)
            .await
    }
}

#[cfg(test)]
#[path = "kc_http_tests.rs"]
mod kc_http_tests;
