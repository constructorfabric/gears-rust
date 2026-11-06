//! Domain ports implemented by infrastructure adapters.

use std::sync::Arc;

use async_trait::async_trait;
use authn_resolver_sdk::ClientCredentialsRequest;
use jsonwebtoken::jwk::JwkSet;
use secrecy::SecretString;
use time::OffsetDateTime;
use toolkit_macros::domain_model;
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

/// Exchanged access token and absolute deadlines, preserved across cache hits.
#[domain_model]
#[derive(Clone)]
pub struct ExchangedToken {
    /// The access token (a JWT). Redacted from `Debug`.
    pub access_token: SecretString,
    /// Original OAuth deadline, preserved on lower-cache hits.
    pub expires_at: Option<OffsetDateTime>,
    /// Original finite local reuse policy; never sliding on a cache hit.
    pub cache_until: OffsetDateTime,
}

impl std::fmt::Debug for ExchangedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExchangedToken")
            .field("expires_at", &self.expires_at)
            .field("cache_until", &self.cache_until)
            .finish_non_exhaustive()
    }
}

/// Exchanges client credentials while retaining credential lifetime.
#[async_trait]
pub trait ClientCredentialsExchanger: Send + Sync {
    /// Perform the client credentials exchange and return the access token.
    async fn exchange(
        &self,
        request: &ClientCredentialsRequest,
        issuer_trust: &IssuerTrustConfig,
    ) -> Result<ExchangedToken, AuthNError>;
}
