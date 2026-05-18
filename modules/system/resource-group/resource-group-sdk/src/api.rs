// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-traits:p1
//! SDK trait contracts for the resource-group module.

use async_trait::async_trait;
use modkit_security::SecurityContext;

use modkit_odata::{ODataQuery, Page};
use uuid::Uuid;

use crate::error::ResourceGroupError;
use crate::models::{
    CreateGroupRequest, CreateTypeRequest, ResourceGroup, ResourceGroupMembership,
    ResourceGroupType, ResourceGroupWithDepth, UpdateGroupRequest, UpdateTypeRequest,
};

/// Client trait for resource-group type management.
///
/// Consumers obtain this from `ClientHub`:
/// ```ignore
/// let client = hub.get::<dyn ResourceGroupClient>()?;
/// let rg_type = client.get_type(&ctx, "gts.cf.core.rg.type.v1~...").await?;
/// ```
#[async_trait]
pub trait ResourceGroupClient: Send + Sync {
    // -- Type lifecycle --

    /// Create a new GTS type definition.
    async fn create_type(
        &self,
        ctx: &SecurityContext,
        request: CreateTypeRequest,
    ) -> Result<ResourceGroupType, ResourceGroupError>;

    /// Get a GTS type definition by its code (GTS type path).
    async fn get_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
    ) -> Result<ResourceGroupType, ResourceGroupError>;

    /// List GTS type definitions with `OData` filtering and cursor-based pagination.
    async fn list_types(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, ResourceGroupError>;

    /// Update a GTS type definition (full replacement).
    async fn update_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
        request: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, ResourceGroupError>;

    /// Delete a GTS type definition. Fails if groups of this type exist.
    async fn delete_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
    ) -> Result<(), ResourceGroupError>;

    // -- Group lifecycle --

    /// Create a new resource group.
    async fn create_group(
        &self,
        ctx: &SecurityContext,
        request: CreateGroupRequest,
    ) -> Result<ResourceGroup, ResourceGroupError>;

    /// Get a resource group by ID.
    async fn get_group(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ResourceGroup, ResourceGroupError>;

    /// List resource groups with `OData` filtering and cursor-based pagination.
    async fn list_groups(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, ResourceGroupError>;

    /// Update a resource group (full replacement).
    async fn update_group(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        request: UpdateGroupRequest,
    ) -> Result<ResourceGroup, ResourceGroupError>;

    /// Delete a resource group (non-cascade).
    ///
    /// The call fails with `ConflictActiveReferences` if the group has child
    /// groups or active memberships. For force-cascade behaviour use
    /// [`Self::delete_group_cascade`].
    async fn delete_group(&self, ctx: &SecurityContext, id: Uuid)
    -> Result<(), ResourceGroupError>;

    /// Force-delete a resource group, cascading into the entire subtree:
    /// every descendant group, every membership row for those groups, and
    /// every closure-table row anchored at this group. Mirrors the
    /// `force=true` REST flag.
    ///
    /// Intended for **cross-module cleanup paths** -- e.g. the AM
    /// tenant-hard-delete cascade hook that tears down all user-group
    /// state for a tenant before the `tenants` row is removed. Most
    /// consumers want [`Self::delete_group`] (the non-cascade variant)
    /// and surface `ConflictActiveReferences` to the caller as 409.
    ///
    /// Default impl delegates to the non-cascade variant so existing
    /// implementers (production `RgService`, test fakes) compile without
    /// breakage; implementations that genuinely support cascade SHOULD
    /// override this to call into their REST-side `force=true` path.
    /// Implementations that cannot cascade (e.g. inert test fakes) are
    /// expected to return `ConflictActiveReferences` from the default
    /// fallback when the group has children / memberships, mirroring the
    /// non-cascade contract.
    async fn delete_group_cascade(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), ResourceGroupError> {
        self.delete_group(ctx, id).await
    }

    /// Get descendants of a reference group (depth >= 0).
    async fn get_group_descendants(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError>;

    /// Get ancestors of a reference group (depth <= 0).
    async fn get_group_ancestors(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError>;

    // -- Membership lifecycle --

    /// Add a membership link between a resource and a group.
    async fn add_membership(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<ResourceGroupMembership, ResourceGroupError>;

    /// Remove a membership link.
    async fn remove_membership(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<(), ResourceGroupError>;

    /// List memberships with `OData` filtering and cursor-based pagination.
    async fn list_memberships(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, ResourceGroupError>;
}

// @cpt-dod:cpt-cf-resource-group-dod-integration-auth-read-service:p1
/// Narrow read-only trait for group data, used by in-process plugin consumers
/// (`AuthZ` resolver plugin, tenant-resolver RG plugin).
///
/// Scope is deliberately "reads only": hierarchy walks anchored at a reference
/// group (ancestors / descendants with depth) **and** flat OData-filtered group
/// listing. Writes remain the responsibility of the full `ResourceGroupClient`.
///
/// The listing method (`list_groups`) is what allows consumers to fetch several
/// groups by id in a single round-trip (`id in (id1, id2, …)`), which is the
/// batch read pattern the tenant-resolver RG plugin uses for
/// `get_tenants(&[TenantId])`.
#[async_trait]
pub trait ResourceGroupReadHierarchy: Send + Sync {
    /// Get descendants of a reference group (depth >= 0).
    async fn get_group_descendants(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError>;

    /// Get ancestors of a reference group (depth <= 0).
    async fn get_group_ancestors(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, ResourceGroupError>;

    /// List resource groups with `OData` filtering and cursor-based pagination.
    ///
    /// Mirrors [`ResourceGroupClient::list_groups`] — a single implementation
    /// on the RG service backs both traits. Exposed on the narrow trait so
    /// plugin consumers can perform batch reads (e.g. `id in (...)` filters)
    /// without pulling in the full client surface.
    async fn list_groups(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, ResourceGroupError>;
}
