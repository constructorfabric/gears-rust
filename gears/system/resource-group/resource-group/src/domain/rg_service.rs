// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-integration-auth-read-service:p1
//! Unified service adapter implementing `ResourceGroupClient` for `ClientHub` registration.
//!
//! Delegates to `GroupService` and `MembershipService` to satisfy
//! the full SDK trait contract.

use std::sync::Arc;

use async_trait::async_trait;
use resource_group_sdk::ResourceGroupClient;
use resource_group_sdk::models::{
    CreateGroupRequest, ResourceGroup, ResourceGroupMembership, ResourceGroupWithDepth,
    UpdateGroupRequest,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_odata::{ODataQuery, Page};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::domain::group_service::GroupService;
use crate::domain::membership_service::MembershipService;
use crate::domain::repo::{GroupRepositoryTrait, MembershipRepositoryTrait};

/// Unified adapter registered with `ClientHub` as `dyn ResourceGroupClient`.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[allow(clippy::struct_field_names)]
pub struct RgService<GR: GroupRepositoryTrait, MR: MembershipRepositoryTrait> {
    group_service: Arc<GroupService<GR>>,
    membership_service: Arc<MembershipService<GR, MR>>,
}

impl<GR: GroupRepositoryTrait, MR: MembershipRepositoryTrait> RgService<GR, MR> {
    /// Create a new `RgService`.
    #[must_use]
    pub fn new(
        group_service: Arc<GroupService<GR>>,
        membership_service: Arc<MembershipService<GR, MR>>,
    ) -> Self {
        Self {
            group_service,
            membership_service,
        }
    }
}

#[async_trait]
impl<GR: GroupRepositoryTrait, MR: MembershipRepositoryTrait> ResourceGroupClient
    for RgService<GR, MR>
{
    // -- Group lifecycle --

    async fn create_group(
        &self,
        ctx: &SecurityContext,
        request: CreateGroupRequest,
    ) -> Result<ResourceGroup, CanonicalError> {
        let tenant_id = ctx.subject_tenant_id();
        self.group_service
            .create_group(ctx, request, tenant_id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_group(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ResourceGroup, CanonicalError> {
        self.group_service
            .get_group(ctx, id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_groups(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, CanonicalError> {
        self.group_service
            .list_groups(ctx, query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn update_group(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        request: UpdateGroupRequest,
    ) -> Result<ResourceGroup, CanonicalError> {
        self.group_service
            .update_group(ctx, id, request)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_group(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), CanonicalError> {
        // Non-cascade variant: surface `ConflictActiveReferences` to the
        // caller; cascade goes through `delete_group_cascade` below.
        self.group_service
            .delete_group(ctx, id, false)
            .await
            .map_err(CanonicalError::from)
    }

    async fn delete_group_cascade(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        // Cascade variant: forwards to `delete_group_inner` with
        // `force=true`, which atomically removes the entire subtree,
        // membership rows, and closure rows under a SERIALIZABLE
        // transaction. Mirrors the REST `?force=true` path.
        self.group_service
            .delete_group(ctx, id, true)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_group_descendants(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        self.group_service
            .get_group_descendants(ctx, group_id, query)
            .await
            .map_err(CanonicalError::from)
    }

    async fn get_group_ancestors(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError> {
        self.group_service
            .get_group_ancestors(ctx, group_id, query)
            .await
            .map_err(CanonicalError::from)
    }

    // -- Membership lifecycle --

    async fn add_membership(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<ResourceGroupMembership, CanonicalError> {
        self.membership_service
            .add_membership(ctx, group_id, resource_type, resource_id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn remove_membership(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<(), CanonicalError> {
        self.membership_service
            .remove_membership(ctx, group_id, resource_type, resource_id)
            .await
            .map_err(CanonicalError::from)
    }

    async fn list_memberships(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, CanonicalError> {
        self.membership_service
            .list_memberships(ctx, query)
            .await
            .map_err(CanonicalError::from)
    }
}
