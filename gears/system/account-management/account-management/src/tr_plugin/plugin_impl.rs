//! `TenantResolverPluginClient` implementation backed by AM-owned
//! `tenants` + `tenant_closure` via the `TenantHierarchyReadPort` seam.
//!
//! Each trait method delegates to the matching free function in
//! [`super::queries`], threading the port handle and the
//! platform registry client used for `tenant_type` reverse-resolution.
//! Wiring to AM's `init()` is in the gear's init function.

use std::sync::Arc;

use async_trait::async_trait;
use tenant_resolver_sdk::{
    GetAncestorsOptions, GetAncestorsResponse, GetDescendantsOptions, GetDescendantsResponse,
    GetTenantsOptions, IsAncestorOptions, TenantId, TenantInfo, TenantResolverError,
    TenantResolverPluginClient,
};
use toolkit_security::SecurityContext;
use types_registry_sdk::PlatformTypesRegistryApi;

use crate::domain::tenant::hierarchy_read_port::TenantHierarchyReadPort;

/// In-process TR plugin sharing AM's hierarchy read port and platform registry
/// client for `tenant_type_uuid -> tenant_type` hydration.
///
/// # Trust boundary
///
/// The gateway authorizes calls (DESIGN §4.2). The plugin ignores `_ctx` and
/// reads through the port, whose adapter centrally uses `AccessScope::allow_all()`.
/// Registry hydration is a named platform exception to P33: `tr-authz-plugin`
/// passes `SecurityContext::anonymous()` (gears-rust#1597), not a forwarded user.
/// Revisit when callers provide forwarded or S2S contexts; do not send this
/// anonymous context to the tenant API.
pub struct PluginImpl {
    port: Arc<dyn TenantHierarchyReadPort>,
    types_registry: Arc<dyn PlatformTypesRegistryApi>,
}

impl PluginImpl {
    /// Build the plugin from AM's already-resolved dependencies.
    ///
    /// Called from `AccountManagementGear::init` after the
    /// `TenantHierarchyReadAdapter` and platform registry client have
    /// been constructed.
    #[must_use]
    pub fn new(
        port: Arc<dyn TenantHierarchyReadPort>,
        types_registry: Arc<dyn PlatformTypesRegistryApi>,
    ) -> Self {
        Self {
            port,
            types_registry,
        }
    }
}

#[async_trait]
impl TenantResolverPluginClient for PluginImpl {
    async fn get_tenant(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
    ) -> Result<TenantInfo, TenantResolverError> {
        super::queries::get_tenant(&self.port, &self.types_registry, id).await
    }

    async fn get_root_tenant(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<TenantInfo, TenantResolverError> {
        super::queries::get_root_tenant(&self.port, &self.types_registry).await
    }

    async fn get_tenants(
        &self,
        _ctx: &SecurityContext,
        ids: &[TenantId],
        options: &GetTenantsOptions,
    ) -> Result<Vec<TenantInfo>, TenantResolverError> {
        super::queries::get_tenants(&self.port, &self.types_registry, ids, &options.status).await
    }

    async fn get_ancestors(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        options: &GetAncestorsOptions,
    ) -> Result<GetAncestorsResponse, TenantResolverError> {
        super::queries::get_ancestors(&self.port, &self.types_registry, id, options.barrier_mode)
            .await
    }

    async fn get_descendants(
        &self,
        _ctx: &SecurityContext,
        id: TenantId,
        options: &GetDescendantsOptions,
    ) -> Result<GetDescendantsResponse, TenantResolverError> {
        super::queries::get_descendants(
            &self.port,
            &self.types_registry,
            id,
            options.barrier_mode,
            &options.status,
            options.max_depth,
        )
        .await
    }

    async fn is_ancestor(
        &self,
        _ctx: &SecurityContext,
        ancestor_id: TenantId,
        descendant_id: TenantId,
        options: &IsAncestorOptions,
    ) -> Result<bool, TenantResolverError> {
        super::queries::is_ancestor(&self.port, ancestor_id, descendant_id, options.barrier_mode)
            .await
    }
}
