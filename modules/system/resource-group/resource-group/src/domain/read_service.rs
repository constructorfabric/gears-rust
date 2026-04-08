// @cpt-begin:cpt-cf-resource-group-dod-integration-auth-read-service:p1:inst-full
//! Integration read service for external consumers (e.g., `AuthZ` plugin).
//!
//! Provides a thin adapter over `GroupService` implementing the SDK
//! `ResourceGroupReadHierarchy` and `ResourceGroupReadPluginClient` traits.

// @cpt-dod:cpt-cf-resource-group-dod-integration-auth-read-service:p1
// @cpt-flow:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-1
// Integration read request arrives via ResourceGroupReadHierarchy trait
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-1

use std::sync::Arc;

use async_trait::async_trait;
use modkit_odata::{ODataQuery, Page};
use modkit_security::SecurityContext;
use resource_group_sdk::error::ResourceGroupError;
use resource_group_sdk::models::{ResourceGroupMembership, ResourceGroupWithDepth};
use resource_group_sdk::{ResourceGroupReadHierarchy, ResourceGroupReadPluginClient};
use uuid::Uuid;

use crate::domain::group_service::GroupService;
use crate::domain::membership_service::MembershipService;
use crate::domain::repo::{GroupRepositoryTrait, MembershipRepositoryTrait, TypeRepositoryTrait};

/// Adapter service exposing hierarchy reads via SDK traits.
///
/// Registered with `ClientHub` so that external consumers (`AuthZ` plugin)
/// can resolve `dyn ResourceGroupReadHierarchy` without depending on the
/// module's internal domain types.
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
// RG Module resolves configured provider from module config
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-2
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-3
// IF built-in provider configured (this is the built-in implementation)
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-3
#[async_trait]
impl<GR: GroupRepositoryTrait, TR: TypeRepositoryTrait, MR: MembershipRepositoryTrait>
    ResourceGroupReadHierarchy for RgReadService<GR, TR, MR>
{
    async fn list_group_depth(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError> {
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-3a
        // Route to local persistence path: execute query against RG database
        self.group_service
            .list_group_hierarchy(ctx, group_id, query)
            .await
            .map_err(ResourceGroupError::from)
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-3a
    }
}

// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4
// IF vendor-specific provider configured
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4a
// Resolve plugin instance by configured vendor (this impl delegates to local service)
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4a
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4
#[async_trait]
impl<GR: GroupRepositoryTrait, TR: TypeRepositoryTrait, MR: MembershipRepositoryTrait>
    ResourceGroupReadPluginClient for RgReadService<GR, TR, MR>
{
    async fn list_memberships(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, ResourceGroupError> {
        // @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4b
        // Delegate to ResourceGroupReadPluginClient with SecurityContext passthrough
        self.membership_service
            .list_memberships(ctx, query)
            .await
            .map_err(ResourceGroupError::from)
        // @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-4b
    }
}
// @cpt-begin:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-5
// RETURN results from selected provider
// @cpt-end:cpt-cf-resource-group-flow-integration-auth-plugin-routing:p1:inst-plugin-5
// @cpt-end:cpt-cf-resource-group-dod-integration-auth-read-service:p1:inst-full
