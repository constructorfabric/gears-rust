// Created: 2026-09-06 by Constructor Tech
//! The root tenant, learned from the Tenant Resolver at first use.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use tenant_resolver_sdk::TenantResolverClient;
use toolkit::ClientHub;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::platform_scope::PlatformScope;

/// [`PlatformScope`] over the `ClientHub`.
///
/// The Tenant Resolver is a consumed client: DESIGN.md §4.9 keeps it off this
/// gear's `deps` and has it fetched when first needed rather than during init,
/// so a gear that reads settings during *its* init can never close a dependency
/// cycle through this one. The lookup therefore happens on the first
/// platform-scoped mutation, not at startup.
///
/// The answer is kept for the life of the process. The root tenant is the
/// install-time, undeletable ancestor of every tenant (DESIGN.md §4.7), so a
/// second lookup could only return the same id. Two first callers racing to
/// learn it both get the same answer, and whichever stores it first wins.
pub struct HubPlatformScope {
    hub: Arc<ClientHub>,
    root: OnceLock<Uuid>,
}

impl HubPlatformScope {
    /// Wrap the hub the client will be fetched from.
    #[must_use]
    pub fn new(hub: Arc<ClientHub>) -> Self {
        Self {
            hub,
            root: OnceLock::new(),
        }
    }
}

#[async_trait]
impl PlatformScope for HubPlatformScope {
    async fn root_tenant(&self) -> Result<Uuid, DomainError> {
        if let Some(id) = self.root.get() {
            return Ok(*id);
        }
        let tenants =
            self.hub
                .get::<dyn TenantResolverClient>()
                .map_err(|e| DomainError::Unavailable {
                    detail: format!("tenant resolver: {e}"),
                })?;
        let root = tenants
            .get_root_tenant(&SecurityContext::anonymous())
            .await
            .map_err(|e| DomainError::Unavailable {
                detail: format!("root tenant: {e}"),
            })?;
        // Two first callers racing here both learned the same id, so whichever
        // stored it first is as right as the other; nothing to reconcile.
        Ok(*self.root.get_or_init(|| root.id.0))
    }
}

#[cfg(test)]
#[path = "platform_scope_tests.rs"]
mod platform_scope_tests;
