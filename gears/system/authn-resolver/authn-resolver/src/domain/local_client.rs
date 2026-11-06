//! Local (in-process) client for the `AuthN` resolver.

use std::sync::Arc;

use async_trait::async_trait;
use authn_resolver_sdk::{
    AuthNResolverClient, AuthNResolverError, AuthenticationRequest, AuthenticationResult,
    ClientCredentialsRequest,
};
use toolkit_macros::domain_model;

use super::{DomainError, Service};

/// Local client wrapping the service.
///
/// Registered in `ClientHub` by the gear during `init()`.
#[domain_model]
pub struct AuthNResolverLocalClient {
    svc: Arc<Service>,
}

impl AuthNResolverLocalClient {
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

fn log_and_convert(op: &str, e: DomainError) -> AuthNResolverError {
    tracing::error!(operation = op, error = ?e, "authn_resolver call failed");
    e.into()
}

#[async_trait]
impl AuthNResolverClient for AuthNResolverLocalClient {
    async fn authenticate(
        &self,
        request: &AuthenticationRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.svc
            .authenticate(request)
            .await
            .map_err(|e| log_and_convert("authenticate", e))
    }

    async fn exchange_client_credentials(
        &self,
        request: &ClientCredentialsRequest,
    ) -> Result<AuthenticationResult, AuthNResolverError> {
        self.svc
            .exchange_client_credentials(request)
            .await
            .map_err(|e| log_and_convert("exchange_client_credentials", e))
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "local_client_tests.rs"]
mod local_client_tests;
