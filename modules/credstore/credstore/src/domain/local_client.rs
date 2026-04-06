//! Local (in-process) client for the credstore module.

use std::sync::Arc;

use async_trait::async_trait;
use credstore_sdk::{CredStoreClientV1, CredStoreError, GetSecretResponse, SecretRef};
use modkit_macros::domain_model;
use modkit_security::SecurityContext;

use super::{DomainError, Service};

/// Local client wrapping the credstore service.
///
/// Registered in `ClientHub` by the credstore module during `init()`.
#[domain_model]
pub struct CredStoreLocalClient {
    svc: Arc<Service>,
}

impl CredStoreLocalClient {
    /// Creates a new local client wrapping the given service.
    #[must_use]
    pub fn new(svc: Arc<Service>) -> Self {
        Self { svc }
    }
}

fn log_and_convert(op: &str, e: DomainError) -> CredStoreError {
    match &e {
        DomainError::NotFound => {
            tracing::debug!(operation = op, "credstore secret not found");
        }
        _ => {
            tracing::error!(operation = op, error = ?e, "credstore call failed");
        }
    }
    e.into()
}

#[async_trait]
impl CredStoreClientV1 for CredStoreLocalClient {
    async fn get(
        &self,
        ctx: &SecurityContext,
        key: &SecretRef,
    ) -> Result<Option<GetSecretResponse>, CredStoreError> {
        self.svc
            .get(ctx, key)
            .await
            .map_err(|e| log_and_convert("get", e))
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "local_client_tests.rs"]
mod tests;
