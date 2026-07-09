// Created: 2026-04-16 by Constructor Tech
// Updated: 2026-04-28 by Constructor Tech
// @cpt-begin:cpt-cf-resource-group-dod-integration-auth-read-service:p1:inst-full
//! Integration read service for external consumers (e.g., `AuthZ` plugin).
//!
//! Provides a thin adapter over `GroupService` implementing the SDK
//! `ResourceGroupReadHierarchy` trait.

// @cpt-dod:cpt-cf-resource-group-dod-integration-auth-read-service:p1
// @cpt-flow:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1
// @cpt-flow:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-1
// Integration read request arrives via ResourceGroupReadHierarchy trait
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-1

use std::sync::Arc;

use async_trait::async_trait;
use resource_group_sdk::ResourceGroupReadHierarchy;
use resource_group_sdk::models::{ResourceGroup, ResourceGroupMembership, ResourceGroupWithDepth};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::group_service::GroupService;
use crate::domain::membership_service::MembershipService;
use crate::domain::repo::{GroupRepositoryTrait, MembershipRepositoryTrait, TypeRepositoryTrait};

/// Adapter service exposing hierarchy reads via SDK traits.
///
/// **Bypasses `AuthZ` enforcement** — delegates to `GroupService` unscoped
/// methods which use `AccessScope::allow_all()`. This is by design
/// (see DESIGN §3.6): `AuthZ` plugin is the caller, and it cannot evaluate
/// itself (circular dependency). The in-process `ClientHub` path therefore
/// skips `AuthZ`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
pub struct RgReadService<
    GR: GroupRepositoryTrait,
    TR: TypeRepositoryTrait,
    MR: MembershipRepositoryTrait,
> {
    group_service: Arc<GroupService<GR, TR>>,
    membership_service: Arc<MembershipService<GR, TR, MR>>,
}

impl<GR: GroupRepositoryTrait, TR: TypeRepositoryTrait, MR: MembershipRepositoryTrait>
    RgReadService<GR, TR, MR>
{
    /// Create a new `RgReadService`.
    #[must_use]
    pub fn new(
        group_service: Arc<GroupService<GR, TR>>,
        membership_service: Arc<MembershipService<GR, TR, MR>>,
    ) -> Self {
        Self {
            group_service,
            membership_service,
        }
    }
}

// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-2
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-1
// RG Gear resolves configured provider from gear config; AuthZ plugin
// resolves `dyn ResourceGroupReadHierarchy` from `ClientHub` (registered in
// `gear.rs::init`). The provider trait registered here is the routing point.
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-1
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-2
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-3
// IF built-in provider configured (this is the built-in implementation)
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-3
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4
// IF vendor-specific provider configured — currently no vendor provider is
// wired in this monolith; vendor selection would replace the registered
// `dyn ResourceGroupReadHierarchy` implementation at gear init. The
// fallthrough is the built-in `RgReadService` below.
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4a
// Resolve plugin instance by configured vendor via types-registry (scoped by
// GTS instance ID) — performed at gear init when vendor config is present.
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4a
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4b
// Delegate to ResourceGroupReadPluginClient with SecurityContext passthrough —
// the SecurityContext threaded into trait methods (`_ctx` below) is the
// passthrough vehicle when a vendor implementation is plugged in.
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4b
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4
#[async_trait]
impl<GR: GroupRepositoryTrait, TR: TypeRepositoryTrait, MR: MembershipRepositoryTrait>
    ResourceGroupReadHierarchy for RgReadService<GR, TR, MR>
{
    async fn get_group_descendants(
        &self,
        _ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-3a
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-5
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-2
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-3
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-4
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-5
        // Bypass AuthZ — use unscoped method (AccessScope::allow_all).
        // AuthZ plugin is the caller; it cannot evaluate itself.
        // Plugin invokes `list_group_depth(system_ctx, group_id, query)`;
        // RgReadService delegates to GroupService unscoped read methods which
        // execute the closure-table query and return `Page<ResourceGroupWithDepth>`.
        self.group_service
            .get_group_descendants_unscoped(group_id, query)
            .await
            .map_err(CanonicalError::from)
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-5
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-4
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-3
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-read:p1:inst-plugin-read-2
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-5
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-3a
    }

    async fn get_group_ancestors(
        &self,
        _ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        // Bypass AuthZ — use unscoped method (AccessScope::allow_all).
        // Tenant-resolver plugin needs full ancestor visibility regardless
        // of caller's tenant scope. Confirmed: TR plugins ignore SecurityContext
        // (Acronis/Virtuozzo, 2026-04-17).
        self.group_service
            .get_group_ancestors_unscoped(group_id, query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_groups(
        &self,
        _ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, CanonicalError> {
        // Bypass AuthZ — same rationale as the hierarchy reads above.
        // Used by the tenant-resolver RG plugin's batch `get_tenants` path,
        // which queries `id in (…)` over tenant-typed groups regardless of
        // the caller's tenant scope.
        self.group_service
            .list_groups_unscoped(query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_group(
        &self,
        _ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ResourceGroup, CanonicalError> {
        // Bypass AuthZ — an in-process PDP consumes this for scope-existence
        // checks while acting as the PDP, so it cannot re-enter the enforcer.
        // The consumer reads the group and compares `tenant_id` itself.
        self.group_service
            .get_group_unscoped(id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_memberships(
        &self,
        _ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, CanonicalError> {
        // Bypass AuthZ — an in-process PDP resolves a subject's group
        // memberships while acting as the PDP; re-entering the enforcer would
        // recurse. The caller supplies the subject/tenant OData filter.
        self.membership_service
            .list_memberships_unscoped(query)
            .await
            .map_err(CanonicalError::from)
    }
}
// @cpt-end:cpt-cf-resource-group-dod-integration-auth-read-service:p1:inst-full
