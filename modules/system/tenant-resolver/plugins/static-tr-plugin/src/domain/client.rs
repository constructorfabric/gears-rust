//! Client implementation for the static tenant resolver plugin.
//!
//! Implements `TenantResolverPluginClient` using the domain service.

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
        _ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        self.tenants
            .get(&id)
            .cloned()
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        ids: &[TenantId],
        options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        let mut result = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for id in ids {
            if !seen.insert(id) {
                continue; // Skip duplicate IDs
            }
            if let Some(tenant) = self.tenants.get(id)
                && matches_status(tenant, &options.status)
            {
                result.push(tenant.clone());
            }
            // Missing IDs are silently skipped
        }

        Ok(result)
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        // Get the tenant first
        let tenant = self
            .tenants
            .get(&id)
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })?;

        // Collect ancestors
        let ancestors = self.collect_ancestors(id, options.barrier_mode);

        Ok(GetAncestorsResponse {
            tenant: tenant.into(),
            ancestors,
        })
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        // Get the tenant first (filter does NOT apply to the starting tenant)
        let tenant = self
            .tenants
            .get(&id)
            .ok_or(TenantResolverError::TenantNotFound { tenant_id: id })?;

        // Collect descendants with filter applied during traversal:
        // - Results are in pre-order (parent before children)
        // - Nodes that don't pass filter are excluded along with their subtrees
        let descendants =
            self.collect_descendants(id, &options.status, options.barrier_mode, options.max_depth);

        Ok(GetDescendantsResponse {
            tenant: tenant.into(),
            descendants,
        })
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        self.is_ancestor_of(ancestor_id, descendant_id, options.barrier_mode)
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod tests;
