//! Consumer-side domain service that fulfils requests by delegating to the
//! `PaymentApi` contract resolved from the `ClientHub`.

use std::sync::Arc;

use api_contracts_sdk::PaymentApi;
use api_contracts_sdk::models::{ChargeRequest, ChargeResponse};
use toolkit::ClientHub;
use toolkit_canonical_errors::CanonicalError;
use toolkit_security::SecurityContext;

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
        let api = self
            .hub
            .get::<dyn PaymentApi>()
            .map_err(|_| CanonicalError::service_unavailable().create())?;
        api.charge(ctx, req).await
    }
}
