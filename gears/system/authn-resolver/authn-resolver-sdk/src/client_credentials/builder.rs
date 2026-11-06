//! [`ClientCredentialsProviderBuilder`] and the client-credentials acquisition it wires.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use secrecy::SecretString;
use time::OffsetDateTime;

use super::{
    Acquire, ClientCredentialsProvider, ClientCredentialsProviderConfig,
    ClientCredentialsProviderConfigError, random_jitter,
};
use crate::api::AuthNResolverClient;
use crate::error::AuthNResolverError;
use crate::models::{AuthenticationResult, ClientCredentialsRequest, RequestCacheControl};
use crate::scopes::ScopeSet;

/// Configures a [`ClientCredentialsProvider`] for one set of client credentials.
///
/// Created by [`ClientCredentialsProvider::builder`]. Nothing is cached, spawned or
/// exchanged until [`Self::build`] and the first `get`. Defaults: the
/// `IdP` default scopes and [`ClientCredentialsProviderConfig::default`].
///
/// `Debug` omits the client secret.
#[must_use]
pub struct ClientCredentialsProviderBuilder {
    client: Arc<dyn AuthNResolverClient>,
    client_id: String,
    client_secret: SecretString,
    default_scopes: Option<ScopeSet>,
    config: ClientCredentialsProviderConfig,
}

impl ClientCredentialsProviderBuilder {
    pub(super) fn new(
        client: Arc<dyn AuthNResolverClient>,
        client_id: String,
        client_secret: SecretString,
    ) -> Self {
        Self {
            client,
            client_id,
            client_secret,
            default_scopes: None,
            config: ClientCredentialsProviderConfig::default(),
        }
    }

    /// Scopes requested by [`ClientCredentialsProvider::get`], in canonical form.
    /// [`ClientCredentialsProvider::get_with_scopes`] with an equal set shares their
    /// cache entry. Without this, `get` requests the `IdP` default scopes.
    pub fn with_default_scopes(mut self, scopes: ScopeSet) -> Self {
        self.default_scopes = Some(scopes);
        self
    }

    /// Replace [`ClientCredentialsProviderConfig::default`].
    pub fn with_config(mut self, config: ClientCredentialsProviderConfig) -> Self {
        self.config = config;
        self
    }

    /// Create the provider.
    ///
    /// # Errors
    ///
    /// [`ClientCredentialsProviderConfigError`] if the configuration is invalid.
    pub fn build(self) -> Result<ClientCredentialsProvider, ClientCredentialsProviderConfigError> {
        let acquirer = ClientCredentialsAcquirer {
            client: self.client,
            client_id: self.client_id,
            client_secret: self.client_secret,
        };
        ClientCredentialsProvider::from_acquirer(
            Arc::new(acquirer),
            self.default_scopes,
            self.config,
            Arc::new(OffsetDateTime::now_utc),
            Arc::new(random_jitter),
        )
    }
}

impl fmt::Debug for ClientCredentialsProviderBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientCredentialsProviderBuilder")
            .field("client_id", &self.client_id)
            .field("default_scopes", &self.default_scopes)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// `OAuth2` client-credentials acquisition through the resolver.
struct ClientCredentialsAcquirer {
    client: Arc<dyn AuthNResolverClient>,
    client_id: String,
    client_secret: SecretString,
}

#[async_trait]
impl Acquire for ClientCredentialsAcquirer {
    async fn acquire(
        &self,
        scopes: Option<&ScopeSet>,
        cache_control: RequestCacheControl,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        // The slot key itself is what the plugin receives.
        let mut request =
            ClientCredentialsRequest::new(self.client_id.clone(), self.client_secret.clone())
                .with_cache_control(cache_control);
        request.scopes = scopes.cloned();
        self.client.exchange_client_credentials(&request).await
    }
}
