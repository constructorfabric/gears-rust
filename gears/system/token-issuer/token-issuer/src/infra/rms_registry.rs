//! Concrete, `ClientHub`-resolving implementation of the
//! [`RmsAdapterRegistry`] port (defined in [`crate::domain::rms_registry`]).

use std::sync::Arc;

use async_trait::async_trait;
use toolkit::client_hub::ClientHub;

use crate::domain::error::DomainError;
use crate::domain::rms_registry::{AdapterRecord, RmsAdapterRegistry};

/// Lazy, fail-closed [`RmsAdapterRegistry`] backed by the [`ClientHub`].
///
/// The RMS client trait (`ResourceManagementClientV1`) registered in the hub
/// does **not** today expose either fact the OBO gates need: a lookup by GTS ID
/// returning the adapter's OBO grant fields (`obo_callback_enabled` /
/// `obo_scope_allowlist` are dropped from the SDK DTO and the only getter is by
/// `Uuid`), nor a `peer_cert_subject` → GTS-ID mapping. RMS Task B added the
/// storage columns and repo methods, but not a client surface. Wiring this gear
/// to the RMS repo would force a `token-issuer` → `rms` gear dependency and the
/// init cycle the lazy-resolution design exists to avoid; building a speculative
/// cross-gear client is out of scope here.
///
/// So this resolver holds the hub for the eventual lazy wire-up but currently
/// returns `Ok(None)` from both reads — fail-closed. With `obo.enabled = false`
/// the OBO path is inert and never calls it; the gate orchestration is fully
/// exercised against the [`RmsAdapterRegistry`] port via mocks in the service
/// tests. This is the one integration seam the OBO surface leaves open
/// (DESIGN.md § 4.1).
pub struct LazyRmsAdapterRegistry {
    hub: Arc<ClientHub>,
}

impl LazyRmsAdapterRegistry {
    /// Builds the resolver over the gear's [`ClientHub`]. The hub is retained
    /// for the (not-yet-wired) lazy resolution of the RMS adapter-registry
    /// client; see the type-level note.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }

    /// The [`ClientHub`] this resolver will lazily resolve the RMS adapter-
    /// registry client from on first use (mirrors
    /// [`crate::infra::plugin_select::GtsSigningPluginSelector`]).
    ///
    /// Inert today — see the type-level note and the `RmsAdapterRegistry` impl.
    // TODO: wire to the RMS adapter-registry client once RMS exposes
    // `get_adapter_by_gts_id` (with obo_callback_enabled / obo_scope_allowlist)
    // and a peer-cert-subject → GTS-ID mapping over `ResourceManagementClientV1`.
    #[allow(
        dead_code,
        reason = "lazy RMS client seam; inert until RMS exposes the adapter-registry client"
    )]
    fn hub(&self) -> &Arc<ClientHub> {
        &self.hub
    }
}

#[async_trait]
impl RmsAdapterRegistry for LazyRmsAdapterRegistry {
    async fn lookup(&self, _gts_id: &str) -> Result<Option<AdapterRecord>, DomainError> {
        // Fail-closed until the RMS adapter-registry client is wired (see note).
        Ok(None)
    }

    async fn gts_id_by_cert_subject(&self, _subject: &str) -> Result<Option<String>, DomainError> {
        // Fail-closed until the RMS adapter-registry client is wired (see note).
        Ok(None)
    }
}

#[cfg(test)]
#[path = "rms_registry_tests.rs"]
mod tests;
