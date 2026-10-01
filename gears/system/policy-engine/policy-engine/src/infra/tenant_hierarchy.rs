//! [`HierarchyPort`] over the tenant-resolver client, every call bounded by
//! `hierarchy_timeout_ms`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tenant_resolver_sdk::{
    BarrierMode, GetAncestorsOptions, IsAncestorOptions, TenantId, TenantResolverClient,
    TenantResolverError,
};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::ports::{HierarchyPort, PortError};

fn map_error(err: TenantResolverError) -> PortError {
    match err {
        TenantResolverError::TenantNotFound { tenant_id } => {
            PortError::NotFound(tenant_id.to_string())
        }
        other => PortError::Unavailable(format!("tenant resolver: {other}")),
    }
}

/// Tenant-resolver backed hierarchy reads.
pub struct TenantResolverHierarchy {
    client: Arc<dyn TenantResolverClient>,
    timeout: Duration,
}

impl std::fmt::Debug for TenantResolverHierarchy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantResolverHierarchy")
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl TenantResolverHierarchy {
    /// Reads through `client`, giving up after `timeout`.
    #[must_use]
    pub fn new(client: Arc<dyn TenantResolverClient>, timeout: Duration) -> Self {
        Self { client, timeout }
    }
}

#[async_trait]
impl HierarchyPort for TenantResolverHierarchy {
    async fn ancestors(&self, ctx: &SecurityContext, tenant: Uuid) -> Result<Vec<Uuid>, PortError> {
        let options = GetAncestorsOptions {
            barrier_mode: BarrierMode::Respect,
        };
        let response = tokio::time::timeout(
            self.timeout,
            self.client.get_ancestors(ctx, TenantId(tenant), &options),
        )
        .await
        .map_err(|_| PortError::Timeout)?
        .map_err(map_error)?;
        if response.tenant.id.0 != tenant {
            return Err(PortError::Unavailable(
                "tenant resolver answered for a different tenant".to_owned(),
            ));
        }
        let mut chain = vec![tenant];
        chain.extend(response.ancestors.iter().map(|a| a.id.0));
        Ok(chain)
    }

    async fn is_reachable(
        &self,
        ctx: &SecurityContext,
        from_tenant: Uuid,
        target_tenant: Uuid,
        barrier: BarrierMode,
    ) -> Result<bool, PortError> {
        if from_tenant == target_tenant {
            return Ok(true);
        }
        let options = IsAncestorOptions {
            barrier_mode: barrier,
        };
        tokio::time::timeout(
            self.timeout,
            self.client.is_ancestor(
                ctx,
                TenantId(from_tenant),
                TenantId(target_tenant),
                &options,
            ),
        )
        .await
        .map_err(|_| PortError::Timeout)?
        .map_err(map_error)
    }
}
