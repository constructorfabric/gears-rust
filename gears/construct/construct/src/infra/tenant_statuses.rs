//! The platform's tenant resolver, as tenant exit asks it.
//!
//! Looked up in the client hub on every question rather than at boot:
//! Construct asks it for nothing else, so the gear neither waits for it nor
//! fails to start without it, and a resolver that comes up later is found.
//! Without one, no tenant exits.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tenant_resolver_sdk::{TenantResolverClient, TenantResolverError};
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::tenant_exit::{TenantStatus, TenantStatuses};

/// How long the resolver may take to answer one question.
const RESOLVER_TIMEOUT: Duration = Duration::from_secs(10);

/// [`TenantStatuses`] over the tenant resolver in the client hub.
///
/// @cpt-dod:cpt-cf-construct-dod-retention-tenant-exit:p2
pub struct HubTenantStatuses {
    hub: Arc<ClientHub>,
    timeout: Duration,
    /// Whether a missing resolver was logged already: it is said once, not
    /// for every tenant on every run.
    missing_said: AtomicBool,
}

impl HubTenantStatuses {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self {
            hub,
            timeout: RESOLVER_TIMEOUT,
            missing_said: AtomicBool::new(false),
        }
    }

    /// The same, over `resolver` rather than the hub's, answering within
    /// `timeout`; the seam a test uses.
    #[cfg(test)]
    pub(crate) fn over(resolver: Arc<dyn TenantResolverClient>, timeout: Duration) -> Self {
        let hub = Arc::new(ClientHub::new());
        hub.register::<dyn TenantResolverClient>(resolver);
        Self {
            hub,
            timeout,
            missing_said: AtomicBool::new(false),
        }
    }
}

#[async_trait]
impl TenantStatuses for HubTenantStatuses {
    async fn status(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<TenantStatus, DomainError> {
        // @cpt-begin:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-status
        let Some(resolver) = self.hub.try_get::<dyn TenantResolverClient>() else {
            if !self.missing_said.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "construct: no tenant resolver is registered, so a tenant the platform \
                     deletes is not removed until one is"
                );
            }
            return Err(DomainError::NotConfigured(
                "no tenant resolver is registered".to_owned(),
            ));
        };
        let answer = tokio::time::timeout(
            self.timeout,
            resolver.get_tenant(ctx, tenant_resolver_sdk::TenantId(tenant_id)),
        )
        .await
        .map_err(|_| {
            DomainError::Unavailable(format!(
                "the tenant resolver did not answer within {:?}",
                self.timeout
            ))
        })?;
        status_of(answer)
        // @cpt-end:cpt-cf-construct-algo-retention-tenant-exit:p2:inst-exit-status
    }
}

/// The resolver's answer as a status, or as a domain error.
fn status_of(
    answer: Result<tenant_resolver_sdk::TenantInfo, TenantResolverError>,
) -> Result<TenantStatus, DomainError> {
    match answer {
        Ok(info) if info.status == tenant_resolver_sdk::TenantStatus::Deleted => {
            Ok(TenantStatus::Deleted)
        }
        Ok(_) => Ok(TenantStatus::Present),
        // Never existed, removed for good, or not shown to Construct: the
        // resolver does not say which, so nothing says it was deleted.
        Err(TenantResolverError::TenantNotFound { .. }) => Ok(TenantStatus::Unknown),
        Err(
            error @ (TenantResolverError::NoPluginAvailable
            | TenantResolverError::ServiceUnavailable(_)),
        ) => Err(DomainError::Unavailable(format!(
            "the tenant resolver is unavailable: {error}"
        ))),
        Err(error) => Err(DomainError::internal(format!(
            "the tenant resolver failed: {error}"
        ))),
    }
}

#[cfg(test)]
#[path = "tenant_statuses_test.rs"]
mod tests;
