//! Port for reading the RMS infrastructure-adapter registry.
//!
//! The token-issuer's OBO re-mint path needs two registry facts: whether a
//! given adapter (by GTS ID) is active and OBO-enabled with its scope
//! allowlist, and which adapter a verified peer certificate subject maps to.
//! This module defines only the *port* and its value type; the concrete,
//! lazily-`ClientHub`-resolving implementation lives in
//! [`crate::infra::rms_registry`] (avoids the token-issuer↔RMS init cycle).

use async_trait::async_trait;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;

/// Registry facts about a single infrastructure adapter, as needed by the OBO
/// re-mint gates.
#[domain_model]
#[derive(Debug, Clone)]
pub struct AdapterRecord {
    /// Whether the adapter is in an active lifecycle state.
    pub status_active: bool,
    /// Whether an operator has granted this adapter OBO callbacks.
    pub obo_callback_enabled: bool,
    /// The operator-granted OBO scope allowlist (subset of the manifest-declared
    /// scopes; enforced on the RMS side at grant time).
    pub obo_scope_allowlist: Vec<String>,
}

/// Read port over the RMS infrastructure-adapter registry.
#[async_trait]
pub trait RmsAdapterRegistry: Send + Sync {
    /// Looks up an adapter by its GTS ID. Returns `Ok(None)` when no such
    /// adapter is registered.
    ///
    /// # Errors
    /// Returns [`DomainError`] if the registry read fails.
    async fn lookup(&self, gts_id: &str) -> Result<Option<AdapterRecord>, DomainError>;

    /// Resolves a verified client-certificate subject to the GTS ID of the
    /// adapter that registered it. Returns `Ok(None)` when no adapter claims
    /// that subject.
    ///
    /// # Errors
    /// Returns [`DomainError`] if the registry read fails.
    async fn gts_id_by_cert_subject(&self, subject: &str) -> Result<Option<String>, DomainError>;
}
