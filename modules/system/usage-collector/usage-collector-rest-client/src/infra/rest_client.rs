//! REST client wiring and [`usage_collector_sdk::UsageCollectorClientV1`] implementation.

use std::sync::Arc;

use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverClient, ClientCredentialsRequest};
use http::StatusCode;
use http_body_util::BodyExt;
use modkit_http::{
    HttpClient, HttpClientBuilder, HttpClientConfig, HttpError, HttpResponse, InvalidUriKind,
};
use tower::ServiceExt;
use tracing::debug;
use url::Url;
use usage_collector_sdk::models::UsageRecord;
use usage_collector_sdk::{
    ModuleConfig, ModuleConfigError, UsageCollectorClientV1, UsageCollectorError, UsageRecordError,
};

use crate::config::UsageCollectorRestClientConfig;
use crate::infra::BearerTokenAuthLayer;

// @cpt-dod:cpt-cf-usage-collector-dod-rest-ingest-rest-client-crate:p1
/// REST-backed [`usage_collector_sdk::UsageCollectorClientV1`].
///
/// The configured `collector_url` is treated as a **base URL** whose path is
/// preserved as a prefix; the API segments (`usage-collector/v1/...`) are
/// appended to it. So `https://gw.example.com/usage/` resolves records to
/// `https://gw.example.com/usage/usage-collector/v1/records`.
pub struct UsageCollectorRestClient {
    records_url: Url,
    modules_prefix: Url,
    http_client: HttpClient,
}

impl UsageCollectorRestClient {
    /// Build a client from module config and the shared `AuthN` resolver.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP client cannot be constructed, or if
    /// `collector_url` is not hierarchical (caught earlier by
    /// [`UsageCollectorRestClientConfig::validate`]).
    pub fn new(
        cfg: &UsageCollectorRestClientConfig,
        authn_client: Arc<dyn AuthNResolverClient>,
    ) -> Result<Self, modkit_http::HttpError> {
        let credentials = ClientCredentialsRequest {
            client_id: cfg.oauth.client_id.clone(),
            client_secret: cfg.oauth.client_secret.clone(),
            scopes: cfg.oauth.scopes.clone(),
        };
        let layer = BearerTokenAuthLayer::new(authn_client, credentials);
        let http_client = HttpClientBuilder::with_config(HttpClientConfig::default())
            .with_auth_layer(move |svc| {
                tower::ServiceBuilder::new()
                    .layer(layer)
                    .service(svc)
                    .boxed_clone()
            })
            .build()?;

        let records_url = build_url(&cfg.collector_url, &["usage-collector", "v1", "records"])?;
        let modules_prefix = build_url(&cfg.collector_url, &["usage-collector", "v1", "modules"])?;

        Ok(Self {
            records_url,
            modules_prefix,
            http_client,
        })
    }
}

/// Appends `segments` to `base`'s path, preserving any existing prefix.
///
/// A trailing empty segment in `base` (e.g. `https://host/prefix/`) is removed
/// before appending so the result is `https://host/prefix/<seg1>/<seg2>/...`
/// rather than `https://host/prefix//<seg1>/...`.
///
/// Returns [`HttpError::InvalidUri`] (kind [`InvalidUriKind::ParseError`]) if
/// `base` is a `cannot_be_a_base` URL (e.g. `data:`), so the caller can
/// propagate a structured error without re-encoding diagnostic strings.
fn build_url(base: &Url, segments: &[&str]) -> Result<Url, HttpError> {
    let mut url = base.clone();
    {
        let mut path = url
            .path_segments_mut()
            .map_err(|()| HttpError::InvalidUri {
                url: base.to_string(),
                kind: InvalidUriKind::ParseError,
                reason: "collector_url cannot be a base".to_owned(),
            })?;
        path.pop_if_empty().extend(segments);
    }
    Ok(url)
}

// @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-6
/// Maps a transport-layer [`HttpError`] to a [`UsageCollectorError`].
///
/// Shared by both [`UsageCollectorClientV1::create_usage_record`] and
/// [`UsageCollectorClientV1::get_module_config`] so the classification stays
/// in lockstep when new [`HttpError`] variants are added. The `deadline`
/// closure is the only call-site-specific behaviour: it builds the
/// `DeadlineExceeded` from the resource-scoped error type
/// ([`UsageRecordError`] for the records flow, [`ModuleConfigError`] for the
/// config flow), preserving the canonical resource type in the GTS prefix.
fn map_transport_err(
    e: HttpError,
    deadline: impl FnOnce(&str) -> UsageCollectorError,
) -> UsageCollectorError {
    match e {
        // `BearerTokenAuthLayer` wraps every AuthN resolver failure (transient or
        // permanent credential rejection) as `HttpError::Transport`, so this arm
        // returns the `ServiceUnavailable` mapping for `inst-rem-3a`/`inst-rem-4a`.
        // Genuine HTTP transport errors (connection refused, DNS failure, TLS
        // failure, …) flow through the same arm and satisfy `inst-rem-7a`.
        HttpError::Transport(_)
        | HttpError::Tls(_)
        | HttpError::Overloaded
        | HttpError::ServiceClosed => UsageCollectorError::service_unavailable()
            .with_detail(format!("REST request failed: {e}"))
            .create(),
        // Timeout variants map to DeadlineExceeded to keep the circuit-breaker
        // semantics intact across both endpoints.
        HttpError::Timeout(_) | HttpError::DeadlineExceeded(_) => {
            deadline("HTTP request deadline exceeded")
        }
        // Permanent client-side / encoding bugs (BodyTooLarge, InvalidHeaderValue,
        // InvalidUri, Json, FormEncode, …) would fail the same way on every
        // retry. Map to `Internal` so the outbox dead-letters instead of looping
        // forever on a poison record.
        other => UsageCollectorError::internal(format!("REST request failed: {other}")).create(),
    }
}
// @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-6

/// Maps a non-success HTTP `status` from the usage collector to a
/// [`UsageCollectorError`].
///
/// Shared by both [`UsageCollectorClientV1::create_usage_record`] and
/// [`UsageCollectorClientV1::get_module_config`] so the classification of the
/// status codes both endpoints handle identically (401, 5xx, catch-all) stays
/// in lockstep when new codes (e.g. 503 Retry-After, 408 Request Timeout) need
/// special handling. The `permission_denied` and `resource_exhausted` closures
/// build the resource-scoped variants from [`UsageRecordError`] /
/// [`ModuleConfigError`] so the canonical resource type stays in the GTS prefix.
///
/// Per-endpoint status codes that diverge from this map (e.g. 422 →
/// `InvalidArgument` for records) must be handled by the caller *before*
/// invoking this helper.
fn map_status_err(
    status: StatusCode,
    body: &str,
    permission_denied: impl FnOnce(String) -> UsageCollectorError,
    resource_exhausted: impl FnOnce(String) -> UsageCollectorError,
) -> UsageCollectorError {
    match status {
        // 401 is transient: the bearer token may have expired between acquisition
        // and the request reaching the gateway. The next delivery attempt acquires
        // a fresh token.
        StatusCode::UNAUTHORIZED => UsageCollectorError::service_unavailable()
            .with_detail(format!(
                "usage collector rejected request with HTTP {}: {body}",
                StatusCode::UNAUTHORIZED
            ))
            .create(),
        // 403 is permanent: the gateway PDP denied the forwarder's service identity.
        StatusCode::FORBIDDEN => permission_denied(format!(
            "usage collector rejected request with HTTP {}: {body}",
            StatusCode::FORBIDDEN
        )),
        // 429 and 5xx are transient — mapped to ResourceExhausted/ServiceUnavailable
        // to trigger retry at the outbox level.
        StatusCode::TOO_MANY_REQUESTS => {
            resource_exhausted(format!("rate limit exceeded by usage collector: {body}"))
        }
        s if s.is_server_error() => UsageCollectorError::service_unavailable()
            .with_detail(format!("usage collector returned HTTP {s}: {body}"))
            .create(),
        // Residual 4xx / unexpected statuses are permanent — dead-letter via Internal.
        status => UsageCollectorError::internal(format!(
            "unexpected HTTP status from usage collector: {status}: {body}"
        ))
        .create(),
    }
}

#[async_trait]
impl UsageCollectorClientV1 for UsageCollectorRestClient {
    // @cpt-flow:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1
    async fn create_usage_record(&self, record: UsageRecord) -> Result<(), UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-1
        // inst-dlv-4: called from DeliveryHandler::handle — see delivery_handler.rs

        // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-5
        let response = self
            .http_client
            .post(self.records_url.as_str())
            .json(&record)
            .map_err(|e| {
                UsageCollectorError::internal(format!("failed to serialize usage record: {e}"))
                    .create()
            })?
            .send()
            .await
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-3a
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-4a
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-7a
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-7
            // Transport / TLS / overload / service-closed → ServiceUnavailable (retry).
            // Timeout / DeadlineExceeded → UsageRecordError::deadline_exceeded (retry).
            // Everything else (encoding bugs, oversized bodies) → Internal (dead-letter).
            // Implementation is shared with `get_module_config` via `map_transport_err`.
            .map_err(|e| {
                map_transport_err(e, |msg| UsageRecordError::deadline_exceeded(msg).create())
            })?;
        // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-7
        // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-7a
        // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-4a
        // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-3a

        match response.status() {
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-8
            StatusCode::NO_CONTENT => Ok(()),
            // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-8
            status => {
                let body = truncated_response_body(response).await;
                match status {
                    // inst-dlv-7: 422 specifically signals validation failure on the submitted
                    // record; surface it as `InvalidArgument` so operators triaging see the
                    // semantic class rather than a generic Internal. Endpoint-specific —
                    // the rest of the status map is shared via `map_status_err`.
                    // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-11
                    StatusCode::UNPROCESSABLE_ENTITY => Err(UsageRecordError::invalid_argument()
                        .with_constraint(format!(
                            "usage collector rejected record with HTTP {}: {body}",
                            StatusCode::UNPROCESSABLE_ENTITY
                        ))
                        .create()),
                    // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-11
                    // 401/403/429/5xx/catch-all are mapped via the shared helper. Trace markers:
                    //   inst-rem-9  → 401   (transient ServiceUnavailable)
                    //   inst-rem-11 → 403   (permanent PermissionDenied via UsageRecordError)
                    //               + catch-all 4xx (permanent Internal — the `inst-rem-11a` half)
                    //   inst-rem-10 → 429 + 5xx (transient — retry at outbox)
                    // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-9
                    // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-10
                    // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-11
                    status => Err(map_status_err(
                        status,
                        &body,
                        |reason| {
                            UsageRecordError::permission_denied()
                                .with_reason(reason)
                                .create()
                        },
                        |quota| {
                            UsageRecordError::resource_exhausted(
                                "usage collector rejected request: rate limit exceeded",
                            )
                            .with_quota_violation("requests", quota)
                            .create()
                        },
                    )),
                    // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-11
                    // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-10
                    // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-9
                }
            }
        }
        // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-5
        // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-remote-emit:p1:inst-rem-1
    }

    // @cpt-flow:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2
    async fn get_module_config(
        &self,
        module_name: &str,
    ) -> Result<ModuleConfig, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-1

        // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-3
        let mut url = self.modules_prefix.clone();
        url.path_segments_mut()
            .map_err(|()| {
                UsageCollectorError::internal("collector_url is not a hierarchical URL").create()
            })?
            .extend([module_name, "config"]);
        // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-3

        let response = self
            .http_client
            .get(url.as_str())
            .send()
            .await
            // Mapping is shared with `create_usage_record` via `map_transport_err`;
            // only the deadline factory differs (resource-scoped `ModuleConfigError`).
            .map_err(|e| {
                map_transport_err(e, |msg| ModuleConfigError::deadline_exceeded(msg).create())
            })?;

        match response.status() {
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-4
            StatusCode::OK => response.json::<ModuleConfig>().await.map_err(|e| {
                UsageCollectorError::internal(format!(
                    "failed to parse module config response: {e}"
                ))
                .create()
            }),
            // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-4
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-5
            StatusCode::NOT_FOUND => Err(ModuleConfigError::not_found(format!(
                "module '{module_name}' is not configured"
            ))
            .with_resource(module_name)
            .create()),
            // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-5
            // @cpt-begin:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-6
            status => {
                let body = truncated_response_body(response).await;
                // 401/403/429/5xx/catch-all are mapped via the shared helper so the
                // classification stays in lockstep with `create_usage_record`.
                Err(map_status_err(
                    status,
                    &body,
                    |reason| {
                        ModuleConfigError::permission_denied()
                            .with_reason(reason)
                            .create()
                    },
                    |quota| {
                        ModuleConfigError::resource_exhausted(
                            "usage collector rejected request: rate limit exceeded",
                        )
                        .with_quota_violation("requests", quota)
                        .create()
                    },
                ))
            } // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-6
        }
        // @cpt-end:cpt-cf-usage-collector-flow-rest-ingest-fetch-module-config:p2:inst-cfg-rem-1
    }
}

/// Reads up to 4 KiB of an error response body for diagnostics, then stops.
///
/// Buffering the full body — even when only the first 4 KiB is ever surfaced
/// in the error message — would let a hostile or misbehaving collector pin
/// arbitrarily large allocations on every 4xx/5xx (which is on the retry hot
/// path). Instead we walk the body frame-by-frame, copy at most `MAX` bytes
/// into the diagnostic buffer, and drop the rest by tearing down the response.
///
/// The collected bytes are truncated on a valid UTF-8 boundary before
/// conversion so the resulting string never carries a partial codepoint or
/// a `U+FFFD` replacement injected mid-sequence.
async fn truncated_response_body(response: HttpResponse) -> String {
    const MAX: usize = 4_096;
    let mut body = std::pin::pin!(response.into_body());
    let mut collected: Vec<u8> = Vec::with_capacity(MAX);
    while collected.len() < MAX {
        match body.frame().await {
            Some(Ok(frame)) => {
                if let Some(chunk) = frame.data_ref() {
                    let remaining = MAX - collected.len();
                    let take = chunk.len().min(remaining);
                    collected.extend_from_slice(&chunk[..take]);
                }
            }
            Some(Err(e)) => {
                debug!(error = ?e, "failed to read usage collector response body for diagnostics");
                return format!("<body unreadable: {e}>");
            }
            None => break,
        }
    }
    // Trim trailing bytes that fall inside an incomplete UTF-8 codepoint so the
    // resulting String never carries a partial sequence or a synthesised
    // `U+FFFD` replacement codepoint from `from_utf8_lossy`.
    match std::str::from_utf8(&collected) {
        Ok(s) => s.to_owned(),
        Err(e) => {
            let valid_up_to = e.valid_up_to();
            // `valid_up_to()` returns the prefix length that is guaranteed-valid
            // UTF-8; converting that slice with `from_utf8_lossy` is therefore
            // allocation-only (no replacement codepoints).
            String::from_utf8_lossy(&collected[..valid_up_to]).into_owned()
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "rest_client_tests.rs"]
mod rest_client_tests;
