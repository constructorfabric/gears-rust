// Created: 2026-04-16 by Constructor Tech
// @cpt-dod:cpt-cf-resource-group-dod-sdk-foundation-sdk-traits:p1
//! SDK trait contracts for the resource-group gear.

use async_trait::async_trait;
use toolkit_security::SecurityContext;

use toolkit_odata::{ODataQuery, Page};
use uuid::Uuid;

use toolkit_canonical_errors::CanonicalError;

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
///
/// # Error envelope
///
/// Per [ADR 0005][adr] every fallible method returns
/// `Result<_, CanonicalError>`. The single authoritative AIP-193 ladder
/// (`From<DomainError> for CanonicalError`) lives in the impl crate's
/// `api::rest::error`; this trait surfaces that envelope unchanged.
/// Consumers may propagate it, or opt into the typed
/// [`ResourceGroupError`](crate::ResourceGroupError) projection
/// (`From<CanonicalError>`) for flat dispatch — see its gear docs for
/// the dispatch table and the three integration patterns.
///
/// [adr]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md
#[async_trait]
pub trait ResourceGroupClient: Send + Sync {
    // -- Type lifecycle --

    /// Create a new GTS type definition.
    async fn create_type(
        &self,
        ctx: &SecurityContext,
        request: CreateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError>;

    /// Get a GTS type definition by its code (GTS type path).
    async fn get_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
    ) -> Result<ResourceGroupType, CanonicalError>;

    /// List GTS type definitions with `OData` filtering and cursor-based pagination.
    async fn list_types(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupType>, CanonicalError>;

    /// Update a GTS type definition (full replacement).
    async fn update_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
        request: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError>;

    /// Delete a GTS type definition. Fails if groups of this type exist.
    async fn delete_type(&self, ctx: &SecurityContext, code: &str) -> Result<(), CanonicalError>;

    // -- Group lifecycle --

    /// Create a new resource group.
    async fn create_group(
        &self,
        ctx: &SecurityContext,
        request: CreateGroupRequest,
    ) -> Result<ResourceGroup, CanonicalError>;

    /// Get a resource group by ID.
    async fn get_group(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ResourceGroup, CanonicalError>;

    /// List resource groups with `OData` filtering and cursor-based pagination.
    async fn list_groups(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroup>, CanonicalError>;

    /// Update a resource group (full replacement).
    async fn update_group(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        request: UpdateGroupRequest,
    ) -> Result<ResourceGroup, CanonicalError>;

    /// Delete a resource group (non-cascade).
    ///
    /// The call fails with `FailedPrecondition` (`Subject::ActiveReferences`)
    /// if the group has child
    /// groups or active memberships. For force-cascade behaviour use
    /// [`Self::delete_group_cascade`].
    async fn delete_group(&self, ctx: &SecurityContext, id: Uuid) -> Result<(), CanonicalError>;

    /// Force-delete a resource group, cascading into the entire subtree:
    /// every descendant group, every membership row for those groups, and
    /// every closure-table row anchored at this group. Mirrors the
    /// `force=true` REST flag.
    ///
    /// Intended for **cross-gear cleanup paths** -- e.g. the AM
    /// tenant-hard-delete cascade hook that tears down all user-group
    /// state for a tenant before the `tenants` row is removed. Most
    /// consumers want [`Self::delete_group`] (the non-cascade variant)
    /// and surface `FailedPrecondition` (`Subject::ActiveReferences`) to the caller as 409.
    ///
    /// Default impl delegates to the non-cascade variant so existing
    /// implementers (production `RgService`, test fakes) compile without
    /// breakage; implementations that genuinely support cascade SHOULD
    /// override this to call into their REST-side `force=true` path.
    /// Implementations that cannot cascade (e.g. inert test fakes) are
    /// expected to return `FailedPrecondition` (`Subject::ActiveReferences`) from the default
    /// fallback when the group has children / memberships, mirroring the
    /// non-cascade contract.
    async fn delete_group_cascade(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<(), CanonicalError> {
        self.delete_group(ctx, id).await
    }

    /// Get descendants of a reference group (depth >= 0).
    async fn get_group_descendants(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError>;

    /// Get ancestors of a reference group (depth <= 0).
    async fn get_group_ancestors(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError>;

    // -- Membership lifecycle --

    /// Add a membership link between a resource and a group.
    async fn add_membership(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<ResourceGroupMembership, CanonicalError>;

    /// Remove a membership link.
    async fn remove_membership(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        resource_type: &str,
        resource_id: &str,
    ) -> Result<(), CanonicalError>;

    /// List memberships with `OData` filtering and cursor-based pagination.
    async fn list_memberships(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, CanonicalError>;
}

// @cpt-dod:cpt-cf-resource-group-dod-integration-auth-read-service:p1
/// Narrow read-only trait for group data, used by in-process plugin consumers
/// (`AuthZ` resolver plugin, tenant-resolver RG plugin, and an in-process
/// `AuthZ` PDP).
///
/// Scope is deliberately "reads only": hierarchy walks anchored at a reference
/// group (ancestors / descendants with depth), flat OData-filtered group
/// listing, single-group existence lookup, and membership listing. Writes
/// remain the responsibility of the full `ResourceGroupClient`.
///
/// The listing method (`list_groups`) is what allows consumers to fetch several
/// groups by id in a single round-trip (`id in (id1, id2, …)`), which is the
/// batch read pattern the tenant-resolver RG plugin uses for
/// `get_tenants(&[TenantId])`.
///
/// `get_group` and `list_memberships` back an in-process `AuthZ` PDP's
/// scope-existence checks and group-membership resolution. Such a consumer
/// invokes them while *being* the PDP, so — like the other reads here — they
/// MUST bypass the `PolicyEnforcer`; routing them through it would re-enter the
/// PDP and recurse. Implementations therefore resolve them unscoped (no tenant
/// `AccessScope`); the caller supplies any subject/tenant `OData` filter and
/// owns tenant scoping.
///
/// # Error envelope
///
/// Like [`ResourceGroupClient`], every fallible method returns
/// `Result<_, CanonicalError>` per [ADR 0005]; consumers may project it
/// into the typed [`ResourceGroupError`](crate::ResourceGroupError) view.
///
/// [ADR 0005]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md
#[async_trait]
pub trait ResourceGroupReadHierarchy: Send + Sync {
    /// Get descendants of a reference group (depth >= 0).
    async fn get_group_descendants(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError>;

    /// Get ancestors of a reference group (depth <= 0).
    async fn get_group_ancestors(
        &self,
        ctx: &SecurityContext,
        group_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupWithDepth>, CanonicalError>;

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
    ) -> Result<Page<ResourceGroup>, CanonicalError>;

    /// Get a single resource group by ID (existence + tenant-ownership check).
    ///
    /// Backs PDP scope validation (`/tenants/{t}/resourceGroups/{rg}`): the
    /// consumer reads the group and compares `tenant_id` itself. Resolved
    /// unscoped — see the trait-level note.
    async fn get_group(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<ResourceGroup, CanonicalError>;

    /// List memberships with `OData` filtering and cursor-based pagination.
    ///
    /// Backs PDP group-membership resolution. The caller MUST supply a
    /// subject-scoped filter (e.g. `resource_id eq '<subject_id>'`); omitting it
    /// returns every membership row. Resolved unscoped — see the trait-level note.
    async fn list_memberships(
        &self,
        ctx: &SecurityContext,
        query: &ODataQuery,
    ) -> Result<Page<ResourceGroupMembership>, CanonicalError>;
}
