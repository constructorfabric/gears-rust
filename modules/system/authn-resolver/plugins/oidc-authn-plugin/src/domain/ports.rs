//! Domain ports implemented by infrastructure adapters.

use std::sync::Arc;

use async_trait::async_trait;
use authn_resolver_sdk::ClientCredentialsRequest;
use jsonwebtoken::jwk::JwkSet;
use url::Url;

use crate::config::IssuerTrustConfig;
use crate::domain::error::AuthNError;

/// Provides signing keys for a trusted issuer.
#[async_trait]
pub trait JwksProvider: Send + Sync {
    /// Return the JWKS for `issuer`, using `discovery_base` to resolve metadata
    /// when the backing implementation needs remote discovery.
    async fn get_jwks(&self, issuer: &str, discovery_base: &Url)
    -> Result<Arc<JwkSet>, AuthNError>;

    /// Refresh issuer keys after a key miss.
    async fn force_refresh(
        &self,
        issuer: &str,
        discovery_base: &Url,
    ) -> Result<Arc<JwkSet>, AuthNError>;
}

/// Exchanges client credentials for an access token.
#[async_trait]
pub trait ClientCredentialsExchanger: Send + Sync {
    /// Perform the client credentials exchange and return the access token.
    async fn exchange(
        &self,
        request: &ClientCredentialsRequest,
        issuer_trust: &IssuerTrustConfig,
    ) -> Result<String, AuthNError>;
}
