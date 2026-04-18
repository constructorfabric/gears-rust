//! Client implementation for the RG tenant resolver plugin.
//!
//! Implements `TenantResolverPluginClient` using the domain service.

use std::collections::HashSet;

use async_trait::async_trait;
use modkit_security::SecurityContext;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantResolverError,
    TenantResolverPluginClient, matches_status,
};

use super::service::Service;

#[async_trait]
impl TenantResolverPluginClient for Service {
    async fn get_tenant(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        self.resolve_tenant(ctx, id).await
    }

    async fn get_tenants(
        &self,
        ctx: &SecurityContext,
        ids: &[TenantId],
        options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();

        for id in ids {
            if !seen.insert(id) {
                continue; // Skip duplicate IDs
            }
            match self.resolve_tenant(ctx, *id).await {
                Ok(tenant) if matches_status(&tenant, &options.status) => {
                    result.push(tenant);
                }
                Ok(_) | Err(TenantResolverError::TenantNotFound { .. }) => {
                    // Doesn't match status filter or not found — silently skip
                }
                Err(e) => return Err(e),
            }
        }

        Ok(result)
    }

    async fn get_ancestors(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
        options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        let (tenant, ancestors) =
            self.resolve_ancestors(ctx, id, options.barrier_mode).await?;

        Ok(GetAncestorsResponse { tenant, ancestors })
    }

    async fn get_descendants(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
        options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        let (tenant, descendants) = self
            .resolve_descendants(
                ctx,
                id,
                &options.status,
                options.barrier_mode,
                options.max_depth,
            )
            .await?;

        Ok(GetDescendantsResponse {
            tenant,
            descendants,
        })
    }

    async fn is_ancestor(
        &self,
        ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        self.check_is_ancestor(ctx, ancestor_id, descendant_id, options.barrier_mode)
            .await
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod client_tests;
