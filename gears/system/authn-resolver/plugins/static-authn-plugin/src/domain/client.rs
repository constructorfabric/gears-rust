// Updated: 2026-04-07 by Constructor Tech
//! Client implementation for the static `AuthN` resolver plugin.
//!
//! Implements `AuthNResolverPluginClient` using the domain service.

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverError, AuthNResolverPluginClient, AuthenticationRequest, AuthenticationResult,
    ClientCredentialsRequest,
};
use secrecy::ExposeSecret as _;

use super::service::Service;

#[async_trait]
impl AuthNResolverPluginClient for Service {
    /// No authenticate cache: every call is a fresh lookup, so any
    /// `cache_control` is honoured.
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.authenticate(request.bearer_token.expose_secret())
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
mod client_tests;
