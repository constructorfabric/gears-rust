//! Peer-identity resolution seam for OBO re-mint Gate 1.
//!
//! The re-mint path must bind the presented capability token to the calling
//! peer (`cap.aud == peer GTS-ID`). The peer identity comes from the mTLS
//! client certificate, which the external mTLS layer supplies. Until that lands
//! (DESIGN.md § 4.1) the cert is absent and resolution is fail-closed; the whole OBO
//! surface is gated off, so this is safe.

use std::sync::Arc;

use async_trait::async_trait;
use toolkit_macros::domain_model;

use crate::domain::error::DomainError;
use crate::domain::rms_registry::RmsAdapterRegistry;

/// Connection-level facts about the calling peer, populated by the transport.
#[domain_model]
#[derive(Debug, Clone, Default)]
pub struct PeerConnInfo {
    /// The verified client-certificate subject, when mTLS is in force.
    pub client_cert_subject: Option<String>,
}

/// Resolves a peer connection to its adapter GTS ID.
#[async_trait]
pub trait PeerIdentityResolver: Send + Sync {
    /// Resolves the peer to its adapter GTS ID.
    ///
    /// # Errors
    /// - [`DomainError::PeerUnverified`] when the peer presented no verified
    ///   certificate.
    /// - [`DomainError::PeerUnknown`] when the certificate subject maps to no
    ///   registered adapter.
    /// - other [`DomainError`] on registry failure.
    async fn resolve(&self, peer: &PeerConnInfo) -> Result<String, DomainError>;
}

/// [`PeerIdentityResolver`] backed by the RMS registry: maps the verified
/// client-certificate subject to the adapter's GTS ID. Fail-closed when no
/// certificate is present.
#[domain_model]
pub struct RegistryPeerIdentityResolver {
    registry: Arc<dyn RmsAdapterRegistry>,
}

impl RegistryPeerIdentityResolver {
    /// Builds the resolver over a registry port.
    #[must_use]
    pub fn new(registry: Arc<dyn RmsAdapterRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl PeerIdentityResolver for RegistryPeerIdentityResolver {
    async fn resolve(&self, peer: &PeerConnInfo) -> Result<String, DomainError> {
        let subject = peer
            .client_cert_subject
            .as_deref()
            .ok_or(DomainError::PeerUnverified)?;
        self.registry
            .gts_id_by_cert_subject(subject)
            .await?
            .ok_or(DomainError::PeerUnknown)
    }
}

#[cfg(test)]
#[path = "peer_identity_tests.rs"]
mod tests;
