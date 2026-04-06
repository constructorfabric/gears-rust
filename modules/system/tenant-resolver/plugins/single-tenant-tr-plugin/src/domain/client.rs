//! Client implementation for the single-tenant resolver plugin.
//!
//! Implements `TenantResolverPluginClient` using single-tenant (flat) semantics.
//! In single-tenant mode:
//! - There is only one tenant (the one from the security context)
//! - It has no parent and no children
//! - Hierarchy operations return minimal results

use async_trait::async_trait;
use modkit_security::SecurityContext;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantRef, TenantResolverError,
    TenantResolverPluginClient, TenantStatus, matches_status,
};

use super::service::Service;

// Tenant name for single-tenant mode.
const TENANT_NAME: &str = "Default";

/// Build tenant info for the single-tenant mode.
fn build_tenant_info(id: TenantId) -> TenantInfo {
    TenantInfo {
        id,
        name: TENANT_NAME.to_owned(),
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: None,     // Root tenant (no parent)
        self_managed: false, // Not a barrier
    }
}

/// Build tenant ref for hierarchy operations in single-tenant mode.
fn build_tenant_ref(id: TenantId) -> TenantRef {
    TenantRef {
        id,
        status: TenantStatus::Active,
        tenant_type: None,
        parent_id: None,     // Root tenant (no parent)
        self_managed: false, // Not a barrier
    }
}

#[async_trait]
impl TenantResolverPluginClient for Service {
    async fn get_tenant(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        let ctx_tenant = TenantId(ctx.subject_tenant_id());
        // Reject nil UUID (anonymous context)
        if ctx_tenant.is_nil() {
            return Err(TenantResolverError::TenantNotFound { tenant_id: id });
        }
        // Only return tenant info if ID matches security context
        if id == ctx_tenant {
            Ok(build_tenant_info(id))
        } else {
            Err(TenantResolverError::TenantNotFound { tenant_id: id })
        }
    }

    async fn get_tenants(
        &self,
        ctx: &SecurityContext,
        ids: &[TenantId],
        options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        let ctx_tenant = TenantId(ctx.subject_tenant_id());
        // Nil UUID context means no tenant exists
        if ctx_tenant.is_nil() {
            return Ok(vec![]);
        }

        let mut result = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for id in ids {
            if !seen.insert(id) {
                continue; // Skip duplicate IDs
            }
            // Only the context tenant exists
            if *id == ctx_tenant {
                let tenant = build_tenant_info(*id);
                if matches_status(&tenant, &options.status) {
                    result.push(tenant);
                }
            }
            // Other IDs are silently skipped (they don't exist)
        }

        Ok(result)
    }

    async fn get_ancestors(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
        _options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        let ctx_tenant = TenantId(ctx.subject_tenant_id());
        // Reject nil UUID (anonymous context)
        if ctx_tenant.is_nil() {
            return Err(TenantResolverError::TenantNotFound { tenant_id: id });
        }
        // Only the context tenant exists
        if id != ctx_tenant {
            return Err(TenantResolverError::TenantNotFound { tenant_id: id });
        }

        // In single-tenant mode, the tenant is the root (no ancestors)
        Ok(GetAncestorsResponse {
            tenant: build_tenant_ref(id),
            ancestors: vec![], // No ancestors in flat model
        })
    }

    async fn get_descendants(
        &self,
        ctx: &SecurityContext,
        id: TenantId,
        _options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        let ctx_tenant = TenantId(ctx.subject_tenant_id());
        // Reject nil UUID (anonymous context)
        if ctx_tenant.is_nil() {
            return Err(TenantResolverError::TenantNotFound { tenant_id: id });
        }
        // Only the context tenant exists
        if id != ctx_tenant {
            return Err(TenantResolverError::TenantNotFound { tenant_id: id });
        }

        // In single-tenant mode, there are no descendants
        Ok(GetDescendantsResponse {
            tenant: build_tenant_ref(id),
            descendants: vec![],
        })
    }

    async fn is_ancestor(
        &self,
        ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        _options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        let ctx_tenant = TenantId(ctx.subject_tenant_id());
        // Reject nil UUID (anonymous context)
        if ctx_tenant.is_nil() {
            return Err(TenantResolverError::TenantNotFound {
                tenant_id: ancestor_id,
            });
        }

        // Both must be the context tenant (only one tenant exists)
        if ancestor_id != ctx_tenant {
            return Err(TenantResolverError::TenantNotFound {
                tenant_id: ancestor_id,
            });
        }
        if descendant_id != ctx_tenant {
            return Err(TenantResolverError::TenantNotFound {
                tenant_id: descendant_id,
            });
        }

        // Self is NOT an ancestor of self
        Ok(false)
    }
}
#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "client_tests.rs"]
mod tests;
