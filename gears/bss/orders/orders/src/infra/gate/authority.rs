//! The commercial service authority (D-194, DESIGN `#contract-08-commercial-service-authorization`;
//! Q13): the gate reads Pricing and Products as the Orders service principal configured for the
//! seller, never as the buyer. The principal comes from the platform `AuthN` S2S path,
//! `AuthNResolverClient::exchange_client_credentials`, the call mini-chat makes for its OAGW
//! provisioning context (`gears/mini-chat/mini-chat/src/gear.rs`, `exchange_client_credentials`):
//! the returned `AuthenticationResult::security_context` is the context, never a hand-built one.
//!
//! The token inside the context is never logged or persisted: logs name the seller and the error
//! kind only.
use crate::config::CommercialIdentityConfig;
use async_trait::async_trait;
use authn_resolver_sdk::{AuthNResolverClient, AuthNResolverError, ClientCredentialsRequest};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration as StdDuration;
use toolkit_security::SecurityContext;
use uuid::Uuid;

/// The baseline deadline of the one `AuthN` exchange per `PricingCatalogReads::read_lines` call:
/// the baseline of a gate port (250 ms; D-227 item 5 is unratified), so a hung `IdP` cannot stall
/// an assessment (F03 §3.3). `gate.authority_exchange_deadline_ms` overrides it.
pub const AUTHORITY_EXCHANGE_DEADLINE: StdDuration = StdDuration::from_millis(250);

/// The most sellers a [`WarnOnce`] remembers; past it nothing more is inserted or warned.
const WARNED_CAP: usize = 10_000;

/// A once-per-seller-per-process warning gate with a bounded memory.
#[derive(Debug, Default)]
pub struct WarnOnce {
    seen: parking_lot::Mutex<BTreeSet<Uuid>>,
    exhausted: AtomicBool,
}

impl WarnOnce {
    /// Whether this is the first call for `seller` (the caller should warn). Once
    /// [`WARNED_CAP`] sellers are remembered it returns `false` for every new seller and logs
    /// once that suppression is exhausted.
    pub fn first(&self, seller: Uuid) -> bool {
        let mut seen = self.seen.lock();
        if seen.contains(&seller) {
            return false;
        }
        if seen.len() >= WARNED_CAP {
            drop(seen);
            if !self.exhausted.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    target: "orders.gate.authority",
                    cap = WARNED_CAP,
                    "authority warning suppression is exhausted; further sellers are not warned about"
                );
            }
            return false;
        }
        seen.insert(seller);
        true
    }
}

/// Why no service context exists for a seller.
#[derive(Debug, thiserror::Error)]
pub enum AuthorityError {
    /// No commercial identity is configured for the seller (`gate.commercial_identities`).
    #[error("no commercial identity is provisioned for the seller")]
    NotProvisioned,
    /// The `AuthN` resolver refused or failed the client-credentials exchange.
    #[error("commercial identity exchange failed")]
    Exchange(#[source] AuthNResolverError),
    /// The `AuthN` resolver has no ready plugin.
    #[error("the AuthN resolver is unavailable")]
    Unavailable,
    /// The exchange did not answer within the gate's authority deadline
    /// (`gate.authority_exchange_deadline_ms`).
    #[error("the commercial identity exchange timed out")]
    Deadline,
}

/// The seller-scoped Orders service principal (D-194).
#[async_trait]
pub trait CommercialAuthority: Send + Sync {
    /// The authenticated Orders service principal configured for `seller_tenant_id`.
    ///
    /// # Errors
    /// [`AuthorityError::NotProvisioned`] when no identity is configured for the seller; the
    /// exchange failures otherwise.
    async fn service_context(
        &self,
        seller_tenant_id: Uuid,
    ) -> Result<SecurityContext, AuthorityError>;
}

/// Bounds every exchange of the wrapped authority by one deadline, so a hung `IdP` stalls no
/// caller: the catalog reads, the phase-A attempt freeze and the phase-B re-exchange all share
/// the one bound. Precedent: the per-port deadline of `Budget::bounded`; the platform has none
/// for `exchange_client_credentials` (mini-chat calls it on a startup path).
pub struct DeadlineAuthority {
    inner: Arc<dyn CommercialAuthority>,
    deadline: StdDuration,
}

impl DeadlineAuthority {
    /// `inner`, each exchange bounded by `deadline`.
    #[must_use]
    pub fn new(inner: Arc<dyn CommercialAuthority>, deadline: StdDuration) -> Self {
        Self { inner, deadline }
    }
}

#[async_trait]
impl CommercialAuthority for DeadlineAuthority {
    async fn service_context(&self, seller: Uuid) -> Result<SecurityContext, AuthorityError> {
        tokio::time::timeout(self.deadline, self.inner.service_context(seller))
            .await
            .map_err(|_elapsed| AuthorityError::Deadline)?
    }
}

/// The production authority until commercial grants exist: every seller is not provisioned.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotProvisioned;

#[async_trait]
impl CommercialAuthority for NotProvisioned {
    async fn service_context(&self, _seller: Uuid) -> Result<SecurityContext, AuthorityError> {
        Err(AuthorityError::NotProvisioned)
    }
}

/// The `AuthN` client-credentials authority over `gate.commercial_identities`, keyed by seller.
/// Every call exchanges: `AuthenticationResult` carries no token expiry today
/// (`authn-resolver-sdk/src/models.rs`), so no context is cached; re-add a cache when the SDK
/// exposes one (D-240). Each submit phase resolves its own context (D-194).
pub struct AuthnAuthority {
    resolver: Arc<dyn AuthNResolverClient>,
    identities: BTreeMap<Uuid, CommercialIdentityConfig>,
    /// Sellers already warned about a failed exchange (one warning per seller per process).
    warned: WarnOnce,
}

impl std::fmt::Debug for AuthnAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The secrets are never printed.
        f.debug_struct("AuthnAuthority")
            .field("sellers", &self.identities.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl AuthnAuthority {
    /// An authority presenting `identities` (one per seller; `GateConfig::validate` rejects a
    /// duplicate) through `resolver`.
    #[must_use]
    pub fn new(
        resolver: Arc<dyn AuthNResolverClient>,
        identities: &[CommercialIdentityConfig],
    ) -> Self {
        Self {
            resolver,
            identities: identities
                .iter()
                .map(|i| (i.seller_tenant_id, i.clone()))
                .collect(),
            warned: WarnOnce::default(),
        }
    }
}

#[async_trait]
impl CommercialAuthority for AuthnAuthority {
    async fn service_context(
        &self,
        seller_tenant_id: Uuid,
    ) -> Result<SecurityContext, AuthorityError> {
        let identity = self
            .identities
            .get(&seller_tenant_id)
            .ok_or(AuthorityError::NotProvisioned)?;
        let request = ClientCredentialsRequest {
            client_id: identity.client_id.clone(),
            client_secret: identity.client_secret.clone(),
            scopes: identity.scopes.clone(),
        };
        let result = self
            .resolver
            .exchange_client_credentials(&request)
            .await
            .map_err(|e| {
                let kind = match &e {
                    AuthNResolverError::Unauthorized(_) => "unauthorized",
                    AuthNResolverError::NoPluginAvailable => "no-plugin",
                    AuthNResolverError::ServiceUnavailable(_) => "service-unavailable",
                    AuthNResolverError::TokenAcquisitionFailed(_) => "token-acquisition-failed",
                    AuthNResolverError::Internal(_) => "internal",
                };
                // The error text may echo IdP detail: only the kind is logged.
                if self.warned.first(seller_tenant_id) {
                    tracing::warn!(
                        target: "orders.gate.authority",
                        seller_tenant_id = %seller_tenant_id,
                        kind,
                        "commercial identity exchange failed"
                    );
                }
                match e {
                    AuthNResolverError::NoPluginAvailable
                    | AuthNResolverError::ServiceUnavailable(_) => AuthorityError::Unavailable,
                    other => AuthorityError::Exchange(other),
                }
            })?;
        Ok(result.security_context)
    }
}

/// The production authority for `identities`: [`NotProvisioned`] when the list is empty,
/// [`AuthnAuthority`] over `resolver` otherwise.
///
/// # Errors
/// Identities are configured but no `AuthNResolverClient` is published: a configured seller
/// would silently stay unprovisioned, so the gear refuses to start.
pub fn commercial_authority(
    resolver: Option<Arc<dyn AuthNResolverClient>>,
    identities: &[CommercialIdentityConfig],
) -> anyhow::Result<Arc<dyn CommercialAuthority>> {
    if identities.is_empty() {
        return Ok(Arc::new(NotProvisioned));
    }
    let resolver = resolver.ok_or_else(|| {
        anyhow::anyhow!(
            "bss-orders: gate.commercial_identities configured but no AuthNResolverClient is \
             published"
        )
    })?;
    Ok(Arc::new(AuthnAuthority::new(resolver, identities)))
}
