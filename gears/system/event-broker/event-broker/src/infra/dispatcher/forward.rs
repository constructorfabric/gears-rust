//! Resolve-and-forward seam shared by `proxy::handle`/`router::handle`
//! (`eb-dispatcher-routing`). Any real per-instance routing algorithm beyond
//! random selection (topic-pattern matching, cache-based consumer-group
//! ownership, failover) is #4438's job (design.md D1).

use std::time::Duration;

use axum::extract::Request;
use axum::response::Response;
use cluster_sdk::discovery::DiscoveryFilter;
use pingora_core::connectors::http::v1::Connector;
use toolkit::api::canonical_prelude::CanonicalError;

use super::proxy_client;
use crate::domain::cluster::EventBrokerCluster;

/// Idle-read timeout on the proxied response body (design.md D9): reset on
/// every byte received (heartbeat or data), not tied to total connection
/// duration. 60s sits comfortably above the consumer SDK's own ~50s
/// self-heal window (`DESIGN.md:1825`).
const IDLE_READ_TIMEOUT: Duration = Duration::from_mins(1);

/// Shared state every dispatcher handler needs: the resolved cluster
/// primitives and a pooled Pingora HTTP/1 connector. Attached once via
/// `Extension<Arc<DispatcherState>>` after all dispatcher routes are
/// registered (design.md D7).
pub struct DispatcherState {
    pub(crate) cluster: EventBrokerCluster,
    connector: Connector,
    idle_timeout: Duration,
}

impl DispatcherState {
    #[must_use]
    pub fn new(cluster: EventBrokerCluster) -> Self {
        Self {
            cluster,
            connector: Connector::new(None),
            idle_timeout: IDLE_READ_TIMEOUT,
        }
    }

    /// Overrides the idle-read timeout - test-only, so the close-on-idle
    /// behavior (spec "Idle-timeout on proxied streaming connections") is
    /// verifiable without a real 60s wait.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }
}

/// Which role's `ServiceDiscoveryV1` service name to resolve when forwarding
/// (design.md D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Ingest,
    Delivery,
}

impl Role {
    /// The `ServiceDiscoveryV1` service name this role registers/discovers
    /// under.
    fn service_name(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Delivery => "delivery",
        }
    }
}

/// Resolves an instance of `role` via `ServiceDiscoveryV1` and forwards
/// `req` to it. Both failure cases (no instance registered, resolved
/// instance unreachable, or discovery itself unavailable) construct a
/// `CanonicalError::ServiceUnavailable` with a distinguishing `detail`
/// (design.md D10) - no retry in either path.
///
/// # Errors
/// Returns `CanonicalError::ServiceUnavailable` when no `role` instance is
/// registered, when service discovery itself is unreachable, or when the
/// resolved instance's connection fails.
pub async fn forward(
    role: Role,
    state: &DispatcherState,
    req: Request,
) -> Result<Response, CanonicalError> {
    let name = role.service_name();

    let instances = state
        .cluster
        .service_discovery
        .discover(name, DiscoveryFilter::default())
        .await
        .map_err(|_| {
            CanonicalError::service_unavailable()
                .with_detail(format!(
                    "service discovery unavailable for {name} instances"
                ))
                .create()
        })?;

    if instances.is_empty() {
        return Err(CanonicalError::service_unavailable()
            .with_detail(format!("no {name} instance registered"))
            .create());
    }

    // D8: uniformly random pick among returned instances - a placeholder for
    // real load-balancing / consumer-group-ownership-aware routing, which
    // lands with #4438. TODO(#4438): replace with the real algorithm.
    let picked = &instances[rand::random_range(0..instances.len())];

    proxy_client::proxy(&picked.address, &state.connector, req, state.idle_timeout)
        .await
        .map_err(|_| {
            CanonicalError::service_unavailable()
                .with_detail(format!("resolved {name} instance unreachable"))
                .create()
        })
}
