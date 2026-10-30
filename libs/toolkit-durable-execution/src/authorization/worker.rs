//! Storage seam for background scopes. The engine installs [`Authority`]; tests
//! install fakes that also simulate storage faults.
use crate::authorization::{Authority, ServiceAction};
use crate::store::StoreError;
use async_trait::async_trait;
use toolkit_security::AccessScope;

/// Resolves a fresh access scope for a worker or control-plane action.
#[async_trait]
pub trait WorkerAuthorization: Send + Sync {
    async fn scope(&self, action: ServiceAction) -> Result<AccessScope, StoreError>;
}

#[async_trait]
impl WorkerAuthorization for Authority {
    async fn scope(&self, action: ServiceAction) -> Result<AccessScope, StoreError> {
        self.service(action)
            .await
            .map_err(StoreError::Authorization)
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "../../tests/unit/worker_authorization_tests.rs"]
mod tests;
