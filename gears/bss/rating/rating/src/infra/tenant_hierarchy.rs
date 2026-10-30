//! [`TenantHierarchy`] over tenant-resolver (DESIGN §4.8; `UPSTREAM_REQS` §2.13 G-7): whether a feed
//! entry's tenant lies inside its source's configured scope.
//!
//! Barriers are ignored: a self-managed tenant still lies in its parent's subtree, and its usage
//! still belongs to the source that reads that subtree. A tenant the directory does not know lies
//! in no subtree; its entry is set aside with an alarm, never written to a tenant's records.

use std::sync::Arc;

use async_trait::async_trait;
use tenant_resolver_sdk::{
    BarrierMode, IsAncestorOptions, TenantId, TenantResolverClient, TenantResolverError,
};
use uuid::Uuid;

use crate::authz::service_context;
use crate::domain::error::{Cause, DomainError};
use crate::domain::ports::Dependency;
use crate::domain::usage::reader::TenantHierarchy;
use crate::domain::usage::source::TenantScope;

/// [`TenantHierarchy`] over the tenant-resolver client.
#[derive(Clone)]
pub struct ResolverTenantHierarchy {
    client: Arc<dyn TenantResolverClient>,
}

impl ResolverTenantHierarchy {
    /// The hierarchy `client` reads.
    #[must_use]
    pub fn new(client: Arc<dyn TenantResolverClient>) -> Self {
        Self { client }
    }
}

#[async_trait]
impl TenantHierarchy for ResolverTenantHierarchy {
    async fn within(&self, scope: &TenantScope, tenant: Uuid) -> Result<bool, DomainError> {
        let options = IsAncestorOptions {
            barrier_mode: BarrierMode::Ignore,
        };
        for root in scope.roots() {
            if *root == tenant {
                return Ok(true);
            }
            let ctx = service_context(*root)?;
            match self
                .client
                .is_ancestor(&ctx, TenantId(*root), TenantId(tenant), &options)
                .await
            {
                Ok(true) => return Ok(true),
                Ok(false) | Err(TenantResolverError::TenantNotFound { .. }) => {}
                Err(error) => {
                    return Err(DomainError::DependencyUnavailable {
                        dependency: Dependency::TenantHierarchy,
                        detail: format!("could not tell whether {tenant} lies under {root}"),
                        cause: Some(Cause::new(error)),
                    });
                }
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
#[path = "tenant_hierarchy_tests.rs"]
mod tests;
