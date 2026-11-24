//! The platform's tenant resolver, as the offboarding port asks it.
//!
//! Looked up in the client hub on every question rather than at boot: the
//! gear does not depend on the resolver for anything else, so it neither
//! waits for it nor fails to start without it, and a resolver that comes up
//! later is found. Without one, every offboarding is refused.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tenant_resolver_sdk::{TenantResolverClient, TenantResolverError, TenantStatus};
use toolkit::client_hub::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::offboarding::{TenantDirectory, TenantState};

pub struct HubTenantDirectory {
    hub: Arc<ClientHub>,
}

impl HubTenantDirectory {
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self { hub }
    }
}

#[async_trait]
impl TenantDirectory for HubTenantDirectory {
    async fn state(
        &self,
        ctx: &SecurityContext,
        tenant: Uuid,
        within: Duration,
    ) -> Result<TenantState, DomainError> {
        let Some(resolver) = self.hub.try_get::<dyn TenantResolverClient>() else {
            return Err(DomainError::Unavailable {
                detail: "no tenant resolver is registered".to_owned(),
            });
        };
        // Bounded by what is left of the request's deadline: a resolver that
        // hangs must not hold the request, and its connection, with it.
        let answer = tokio::time::timeout(
            within,
            resolver.get_tenant(ctx, tenant_resolver_sdk::TenantId(tenant)),
        )
        .await
        .map_err(|_| DomainError::Deadline)?;
        match answer {
            Ok(info) if info.status == TenantStatus::Deleted => Ok(TenantState::Deleted),
            Ok(_) => Ok(TenantState::Live),
            // Never existed, already removed for good, or not visible to the
            // caller: the resolver does not say which, so neither may we.
            Err(TenantResolverError::TenantNotFound { .. }) => Ok(TenantState::Unknown),
            Err(TenantResolverError::Unauthorized) => Err(DomainError::AccessDenied),
            Err(
                error @ (TenantResolverError::NoPluginAvailable
                | TenantResolverError::ServiceUnavailable(_)),
            ) => Err(DomainError::Unavailable {
                detail: format!("the tenant resolver is unavailable: {error}"),
            }),
            Err(error) => Err(DomainError::internal(format!(
                "the tenant resolver failed: {error}"
            ))),
        }
    }
}
