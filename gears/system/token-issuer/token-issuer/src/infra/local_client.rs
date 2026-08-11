//! In-process [`TokenIssuerClientV1`] that delegates to the domain [`Service`].

use std::sync::Arc;

use async_trait::async_trait;
use token_issuer_sdk::{
    GrantToken, MintCapabilityRequest, MintGrantRequest, TokenIssuerClientV1, TokenIssuerError,
};
use toolkit_security::SecurityContext;

use crate::domain::service::Service;

/// Local client wrapping the minting [`Service`]; registered into the
/// `ClientHub` so in-process consumers (e.g. RMS in Phase 2) can mint tokens.
pub struct TokenIssuerLocalClient {
    svc: Arc<Service>,
}

impl TokenIssuerLocalClient {
    /// Wraps the service.
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

#[async_trait]
impl TokenIssuerClientV1 for TokenIssuerLocalClient {
    async fn mint_capability(
        &self,
        ctx: &SecurityContext,
        req: MintCapabilityRequest,
    ) -> Result<String, TokenIssuerError> {
        self.svc.mint_capability(ctx, req).await
    }

    async fn mint_grant(
        &self,
        ctx: &SecurityContext,
        req: MintGrantRequest,
    ) -> Result<GrantToken, TokenIssuerError> {
        self.svc.mint_grant(ctx, req).await
    }
}
