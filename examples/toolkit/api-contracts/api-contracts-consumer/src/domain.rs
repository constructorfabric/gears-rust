//! Consumer-side domain service that fulfils requests by delegating to the
//! `PaymentApi` contract resolved from the `ClientHub`.

use std::sync::Arc;

use api_contracts_sdk::PaymentApi;
use api_contracts_sdk::models::{ChargeRequest, ChargeResponse};
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

/// How long the status probe's cache round trip may take.
pub const ROUND_TRIP_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Resolves `PaymentApi` from the hub per call and forwards the request.
///
/// This is the heart of binding-mode-agnostic consumption: the service holds
/// only an `Arc<ClientHub>` and calls `hub.get::<dyn PaymentApi>()`. Whether the
/// resolved implementation is a co-located in-process provider (local-wins) or a
/// generated `PaymentApiRestResolvingClient` wired by the proxy-wiring phase is
/// invisible here.
pub struct ChargeProxyService {
    hub: Arc<ClientHub>,
}

impl ChargeProxyService {
    /// Build the service over a `ClientHub` handle (typically
    /// `ctx.client_hub()`).
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }

    /// Resolve `PaymentApi` from the hub and forward a charge.
    ///
    /// # Errors
    /// Returns `CanonicalError::ServiceUnavailable` when no `PaymentApi` is wired
    /// into the hub yet (provider not ready / not resolved); otherwise propagates
    /// whatever the backend `charge` returns.
    pub async fn place_charge(
        &self,
        ctx: SecurityContext,
        req: ChargeRequest,
    ) -> Result<ChargeResponse, CanonicalError> {
        // Carry the `ClientHubError` into the 503 detail: `NotFound` (nothing
        // registered for the trait) and `TypeMismatch` (registered under a
        // different concrete type) are very different wiring bugs, and both
        // would otherwise collapse into the same opaque "Service temporarily
        // unavailable". The error renders only a Rust type path, so it is safe
        // for the public `Problem` body per the `with_detail` caller contract.
        let api = self.hub.get::<dyn PaymentApi>().map_err(|err| {
            CanonicalError::service_unavailable()
                .with_detail(format!("PaymentApi not resolvable from ClientHub: {err}"))
                .create()
        })?;
        api.charge(ctx, req).await
    }

    /// Round-trip a value through the `event-broker` cluster cache: proof that
    /// Cluster coordination works from this process, whichever process hosts
    /// it. Returns the version the write produced.
    ///
    /// # Errors
    /// Returns `CanonicalError::ServiceUnavailable` when the cache cannot be
    /// resolved, the round trip fails, or it takes longer than
    /// [`ROUND_TRIP_DEADLINE`]. The cause is logged, not returned: a cluster
    /// backend's error text can name its hosts.
    pub async fn cluster_round_trip(&self, tenant: uuid::Uuid) -> Result<u64, CanonicalError> {
        let unavailable = |step: &'static str, err: &dyn std::fmt::Display| {
            tracing::warn!(step, error = %err, "cluster status probe failed");
            CanonicalError::service_unavailable()
                .with_detail(format!("cluster {step}"))
                .create()
        };
        let round_trip = async {
            let cache = cluster_sdk::ClusterCacheV1::resolver(&self.hub)
                .profile(crate::cluster::AuditProfile)
                .resolve()
                .await
                .map_err(|err| unavailable("cache not resolvable", &err))?;
            let key = format!("status-probe/{tenant}");
            cache
                .put(cluster_sdk::cache::PutRequest {
                    key: &key,
                    value: b"ok",
                    ttl: cluster_sdk::cache::Ttl::Of(std::time::Duration::from_secs(60)),
                })
                .await
                .map_err(|err| unavailable("put failed", &err))?;
            let entry = cache
                .get(&key)
                .await
                .map_err(|err| unavailable("get failed", &err))?
                .ok_or_else(|| unavailable("get failed", &"the value just written is absent"))?;
            Ok(entry.version)
        };
        // The remote cluster client sets no per-call deadline, and HTTP/2
        // keepalive does not catch a live server that stops answering.
        tokio::time::timeout(ROUND_TRIP_DEADLINE, round_trip)
            .await
            .unwrap_or_else(|_| {
                Err(unavailable(
                    "round trip timed out",
                    &format_args!("no answer in {ROUND_TRIP_DEADLINE:?}"),
                ))
            })
    }
}
