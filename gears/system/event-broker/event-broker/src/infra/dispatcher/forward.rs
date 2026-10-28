//! Resolve-and-forward seam shared by `proxy::handle`/`router::handle`
//! (`eb-dispatcher-routing`). Any real per-instance routing algorithm beyond
//! random selection (topic-pattern matching, cache-based consumer-group
//! ownership, failover) is #4438's job (design.md D1).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::response::Response;
use toolkit::api::canonical_prelude::CanonicalError;
use toolkit::directory::{DirectoryClient, DirectoryNotFound};
use toolkit_canonical_errors::Http;
use toolkit_http_proxy::{ProxyClient, ProxyClientConfig};

use super::proxy_client;
use crate::api::rest::error::EventBrokerResourceError;
use crate::config::MAX_REQUEST_BODY_BYTES;

/// Idle-read timeout on the proxied response body (design.md D9): reset on
/// every byte received (heartbeat or data), not tied to total connection
/// duration. 60s sits comfortably above the consumer SDK's own ~50s
/// self-heal window (`DESIGN.md:1825`).
const IDLE_READ_TIMEOUT: Duration = Duration::from_mins(1);

/// Bound on the TCP connect to a resolved instance. Unlike the idle-read
/// timeout this covers only the handshake, so a short value cannot cut off a
/// slow-but-live stream; it exists so a silent upstream that accepts nothing
/// cannot hang the handler.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Shared state every dispatcher handler needs: the resolved
/// `DirectoryClient` and a pooled HTTP/1 upstream client. Attached once
/// via `Extension<Arc<DispatcherState>>` after all dispatcher routes are
/// registered (design.md D7).
pub struct DispatcherState {
    directory: Arc<dyn DirectoryClient>,
    client: ProxyClient,
    max_request_body_bytes: usize,
}

/// Build the pooled client: `connect_timeout` bounds the TCP connect,
/// `idle_timeout` the response head and every body read (design.md D9).
///
/// # Errors
/// When the engine cannot build its TLS configuration (e.g. no crypto
/// provider installed in a FIPS build). The dispatcher only speaks cleartext
/// HTTP/1, but the engine builds its TLS configuration up front.
fn build_client(
    connect_timeout: Duration,
    idle_timeout: Duration,
) -> Result<ProxyClient, toolkit_http_proxy::ProxyError> {
    let mut cfg = ProxyClientConfig::default();
    cfg.connect_timeout = connect_timeout;
    cfg.read_timeout = Some(idle_timeout);
    ProxyClient::new(cfg)
}

impl DispatcherState {
    /// # Errors
    /// When the pooled upstream client cannot be built (see `build_client`).
    pub fn new(
        directory: Arc<dyn DirectoryClient>,
    ) -> Result<Self, toolkit_http_proxy::ProxyError> {
        Ok(Self {
            directory,
            client: build_client(UPSTREAM_CONNECT_TIMEOUT, IDLE_READ_TIMEOUT)?,
            max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
        })
    }

    /// Overrides the upstream connect timeout - test-only, so the
    /// hang-on-silent-upstream guard is verifiable without a real 5s wait.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_connect_timeout(mut self, connect_timeout: Duration) -> Self {
        let idle_timeout = self
            .client
            .config()
            .read_timeout
            .unwrap_or(IDLE_READ_TIMEOUT);
        self.client = build_client(connect_timeout, idle_timeout).expect("test client");
        self
    }

    /// Overrides the idle-read timeout - test-only, so the close-on-idle
    /// behavior (spec "Idle-timeout on proxied streaming connections") is
    /// verifiable without a real 60s wait.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        let connect_timeout = self.client.config().connect_timeout;
        self.client = build_client(connect_timeout, idle_timeout).expect("test client");
        self
    }

    /// Overrides the proxied-request-body cap - test-only, so the
    /// reject-oversized / forward-at-limit boundary is verifiable with a small
    /// body instead of pushing a full [`MAX_REQUEST_BODY_BYTES`] payload
    /// through a real loopback forward (fragile on some CI runners).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_max_request_body_bytes(mut self, max_request_body_bytes: usize) -> Self {
        self.max_request_body_bytes = max_request_body_bytes;
        self
    }
}

/// Which role's `DirectoryService` gear name to resolve when forwarding
/// (design.md D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Ingest,
    Delivery,
}

impl Role {
    /// The `DirectoryService` gear name this role registers/discovers
    /// under. Prefixed (`"event-broker-"`) so this intra-gear role
    /// registration can't collide with another gear's real name in
    /// `DirectoryService`'s flat namespace.
    fn service_name(self) -> &'static str {
        match self {
            Self::Ingest => "event-broker-ingest",
            Self::Delivery => "event-broker-delivery",
        }
    }
}

/// Resolves an instance of `role` via `DirectoryService` and forwards `req`
/// to it. Both failure cases (no instance registered, resolved instance
/// unreachable, or the directory itself unavailable) construct a
/// `CanonicalError::ServiceUnavailable` with a distinguishing `detail`
/// (design.md D10) - no retry in either path.
///
/// # Errors
/// Returns `CanonicalError::ServiceUnavailable` when no `role` instance is
/// registered, when the directory itself is unreachable, or when the
/// resolved instance's connection fails.
pub async fn forward(
    role: Role,
    state: &DispatcherState,
    req: Request,
) -> Result<Response, CanonicalError> {
    let name = role.service_name();

    // D8: `resolve_rest_service` round-robins over registered instances - a
    // placeholder for real load-balancing / consumer-group-ownership-aware
    // routing, which lands with #4438. Tracked in #4438: replace with the real
    // algorithm.
    let endpoint = state
        .directory
        .resolve_rest_service(name)
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, gear = name, "directory resolution failed");
            if err.downcast_ref::<DirectoryNotFound>().is_some() {
                CanonicalError::service_unavailable()
                    .with_detail(format!("no {name} instance registered"))
                    .create()
            } else {
                CanonicalError::service_unavailable()
                    .with_detail(format!(
                        "service discovery unavailable for {name} instances"
                    ))
                    .create()
            }
        })?;

    proxy_client::proxy(
        &endpoint.uri,
        &state.client,
        req,
        state.max_request_body_bytes,
    )
    .await
    .map_err(|err| {
        tracing::warn!(error = %err, gear = name, "forwarding to resolved instance failed");
        match err {
            proxy_client::ProxyError::BodyTooLarge { limit } => {
                EventBrokerResourceError::invalid_argument()
                    .with_format(format!("proxied request body exceeds {limit} bytes"))
                    .with_override(Http::status_code(413))
                    .create()
            }
            _ => CanonicalError::service_unavailable()
                .with_detail(format!("resolved {name} instance unreachable"))
                .create(),
        }
    })
}
