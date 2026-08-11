//! `toolkit` gear wiring for the token-issuer: init, the readiness-gated serve
//! loop, and the public REST capability.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::anyhow;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use toolkit::api::OpenApiRegistry;
use toolkit::lifecycle::ReadySignal;
use toolkit::{Gear, GearCtx, RestApiCapability};
use tracing::{info, warn};

/// Initial delay between JWKS warm attempts.
const WARM_RETRY_BASE: Duration = Duration::from_millis(500);
/// Cap on the exponential warm-retry backoff.
const WARM_RETRY_MAX: Duration = Duration::from_secs(15);

use crate::config::TokenIssuerConfig;
use crate::domain::metrics::TokenIssuerMetrics;
use crate::domain::peer_identity::{PeerIdentityResolver, RegistryPeerIdentityResolver};
use crate::domain::rms_registry::RmsAdapterRegistry;
use crate::domain::service::Service;
use crate::infra::local_client::TokenIssuerLocalClient;
use crate::infra::plugin_select::GtsSigningPluginSelector;
use crate::infra::rms_registry::LazyRmsAdapterRegistry;

#[toolkit::gear(
    name = "token-issuer",
    deps = [types_registry],
    capabilities = [rest, stateful],
    lifecycle(entry = "serve", await_ready)
)]
pub struct TokenIssuerGear {
    service: OnceLock<Arc<Service>>,
}

impl Default for TokenIssuerGear {
    fn default() -> Self {
        Self {
            service: OnceLock::new(),
        }
    }
}

impl TokenIssuerGear {
    #[allow(
        clippy::redundant_pub_crate,
        reason = "module-private serve entry-point invoked by the toolkit runtime"
    )]
    #[allow(
        clippy::cognitive_complexity,
        reason = "inflated by the three `tracing::info!` expansions; the retry loop \
                  itself lives in `warm_jwks_until_ready`"
    )]
    pub(crate) async fn serve(
        self: Arc<Self>,
        cancel: CancellationToken,
        ready: ReadySignal,
    ) -> anyhow::Result<()> {
        let svc = self
            .service
            .get()
            .cloned()
            .ok_or_else(|| anyhow!("token-issuer: serve invoked before init"))?;

        if !warm_jwks_until_ready(&svc, &cancel).await {
            info!(target: "token_issuer.lifecycle", "token-issuer cancelled before ready");
            return Ok(());
        }

        ready.notify();
        info!(target: "token_issuer.lifecycle", "token-issuer ready; JWKS warmed");

        cancel.cancelled().await;
        info!(target: "token_issuer.lifecycle", "token-issuer cancelled");
        Ok(())
    }
}

/// Warms the JWKS, retrying with capped exponential backoff until it succeeds or
/// `cancel` fires. Returns `true` once warm, `false` if cancelled first.
///
/// Readiness gate: both signing key(s) readable + JWKS buildable. Fail closed —
/// stay not-ready (serving 503) rather than an empty/invalid JWKS. The signing
/// backend may register asynchronously after boot, so retry instead of giving up
/// on a transient miss; a one-shot warm would brick the issuer permanently on a
/// startup race.
async fn warm_jwks_until_ready(svc: &Service, cancel: &CancellationToken) -> bool {
    let mut backoff = WARM_RETRY_BASE;
    loop {
        match svc.warm_jwks().await {
            Ok(()) => return true,
            Err(e) => {
                warn!(
                    target: "token_issuer.lifecycle",
                    error = %e,
                    retry_in = ?backoff,
                    "token-issuer JWKS warm failed; retrying"
                );
                // Sleep `backoff`, but bail out immediately on cancellation.
                if tokio::time::timeout(backoff, cancel.cancelled())
                    .await
                    .is_ok()
                {
                    return false;
                }
                backoff = backoff.saturating_mul(2).min(WARM_RETRY_MAX);
            }
        }
    }
}

#[async_trait]
impl Gear for TokenIssuerGear {
    #[tracing::instrument(skip_all, fields(module = "token-issuer"))]
    async fn init(&self, ctx: &GearCtx) -> anyhow::Result<()> {
        let cfg: TokenIssuerConfig = ctx.config_expanded()?;
        cfg.validate()
            .map_err(|e| anyhow!("token-issuer config invalid: {e}"))?;
        info!(vendor = %cfg.vendor, "initializing token-issuer module");

        // The selector itself implements SigningClientV1, resolving the scoped
        // plugin client lazily on first use.
        let signer = Arc::new(GtsSigningPluginSelector::new(
            ctx.client_hub(),
            cfg.vendor.clone(),
        ));

        // RMS adapter registry (OBO-grant facts) — resolved lazily from the
        // ClientHub so token-issuer keeps `deps = [types_registry]` and does
        // not take a hard dependency on the `rms` gear (avoids the init cycle).
        // Currently fail-closed; see `LazyRmsAdapterRegistry`.
        let registry: Arc<dyn RmsAdapterRegistry> =
            Arc::new(LazyRmsAdapterRegistry::new(ctx.client_hub()));
        // Peer-identity resolver (mTLS cert subject → adapter GTS ID), over the
        // same registry; fail-closed until the external mTLS layer supplies a
        // verified cert (DESIGN.md § 4.1).
        let peer_resolver: Arc<dyn PeerIdentityResolver> =
            Arc::new(RegistryPeerIdentityResolver::new(Arc::clone(&registry)));

        let metrics = Arc::new(TokenIssuerMetrics::from_global());
        let svc = Arc::new(Service::new(
            signer,
            peer_resolver,
            registry,
            &cfg,
            metrics,
        )?);

        self.service
            .set(Arc::clone(&svc))
            .map_err(|_| anyhow!("{} module already initialized", Self::MODULE_NAME))?;

        let client: Arc<dyn token_issuer_sdk::TokenIssuerClientV1> =
            Arc::new(TokenIssuerLocalClient::new(svc));
        ctx.client_hub()
            .register::<dyn token_issuer_sdk::TokenIssuerClientV1>(client);

        info!("token-issuer module initialized");
        Ok(())
    }
}

impl RestApiCapability for TokenIssuerGear {
    fn register_rest(
        &self,
        _ctx: &GearCtx,
        router: axum::Router,
        openapi: &dyn OpenApiRegistry,
    ) -> anyhow::Result<axum::Router> {
        info!("registering token-issuer REST routes");
        let svc = self
            .service
            .get()
            .cloned()
            .ok_or_else(|| anyhow!("token-issuer Service not initialized"))?;
        let router = crate::api::rest::register_routes(router, openapi, svc);
        info!("token-issuer REST routes registered");
        Ok(router)
    }
}
