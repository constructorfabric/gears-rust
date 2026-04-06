//! Client implementation for the static `AuthN` resolver plugin.
//!
//! Implements `AuthNResolverPluginClient` using the domain service.

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverError, AuthNResolverPluginClient, AuthenticationResult, ClientCredentialsRequest,
};

use super::service::Service;

#[async_trait]
impl AuthNResolverPluginClient for Service {
    async fn authenticate(
        &self,
        bearer_token: &str,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.authenticate(bearer_token)
            .ok_or_else(|| AuthNResolverError::Unauthorized("invalid token".to_owned()))
    }

    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.exchange_client_credentials(request).ok_or_else(|| {
            AuthNResolverError::TokenAcquisitionFailed("invalid client credentials".to_owned())
        })
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod tests;
