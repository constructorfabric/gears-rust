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
/// let rg_type = client.get_type(&ctx, tenant_resource_group_type).await?;
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

// @cpt-dod:cpt-cf-resource-group-dod-type-mgmt-service-crud:p1
/// Narrow, deliberately un-gated trait for GTS type-registry bootstrap,
/// used by another gear's `Gear::init` to register the RG type schemas it
/// owns before the platform's `AuthZ` machinery is reachable.
///
/// `gts_type` is a platform-global table with no `tenant_id` column (see
/// the doc block on `RG_TYPE_RESOURCE` in the impl crate's
/// `domain::type_service`), so a tenant-scoped PDP decision has no row-level
/// column to filter on in the first place; the only thing a gate on this
/// surface could ever mean is a bare permission check.
///
/// More importantly, that bare permission check is structurally
/// unavailable at the point this trait is meant to be called from. This
/// trait exists for **deployment-time bootstrap**: another gear's
/// `Gear::init` (see account-management's `register_user_group_types`)
/// registering the RG types it depends on before it can serve traffic.
/// `init` is the toolkit's "config phase" — types-registry keeps every
/// plugin registration in a private staging buffer and only publishes it
/// in `post_init::switch_to_ready`. A `PolicyEnforcer::access_scope_with`
/// call made during `init` therefore resolves against a types-registry
/// view with zero published instances (`list_instances` returns 0 even for
/// a plugin, such as the static-authz plugin, that registered earlier in
/// the very same init pass), and a PDP that finds no matching plugin fails
/// closed with `PluginNotFound`. Deferring the call to `post_init` does not
/// fix this either: by the time the catalogue is populated, the only actor
/// available to a gear's init path is `system_actor::for_gear_init()`,
/// which is platform-scoped — i.e. a nil-UUID tenant — and the static
/// `AuthZ` plugin rejects nil-tenant callers outright. There is no phase of
/// gear initialization at which a gated call on this surface can both find
/// a plugin and be admitted by it.
///
/// This trait therefore bypasses the `PolicyEnforcer` entirely — the same
/// posture as [`ResourceGroupReadHierarchy`], for the same reason (an
/// in-init or in-process caller that cannot be gated without either
/// recursing or failing closed on a structural technicality, not a
/// legitimate access-control decision). It MUST NOT be exposed through
/// REST, under any configuration: the only sanctioned callers are other
/// gears' `init` paths, resolved from `ClientHub` like any other SDK
/// trait. On the RG side, each method is a thin pass-through to the
/// impl crate's own unscoped `TypeService` methods
/// (`create_type_unscoped` / `get_type_unscoped` / `update_type_unscoped`)
/// — the very same methods RG's own `seed_types` uses to seed its types at
/// its own init, for the identical reason.
///
/// That restriction is not just a promise on this doc comment. `ClientHub`
/// has no notion of lifecycle phase, so nothing stops some later handler
/// or background task from resolving this trait unless the impl closes the
/// window itself: RG's `RgTypeBootstrapService` seals itself once
/// `register_rest` runs, which the toolkit runtime only does after every
/// gear's `init` and `post_init` have completed. Sealing fails every
/// method closed from then on -- including for a caller that resolved and
/// held onto an `Arc` from inside the window, which merely removing the
/// `ClientHub` registration would not reach.
///
/// `ctx` is threaded through for audit correlation only (log/trace
/// enrichment on the impl side) and is never used to enforce anything —
/// exactly the shape [`ResourceGroupReadHierarchy`] uses its `_ctx` for.
///
/// # Temporary — revisit when the type registry becomes its own gear
///
/// This bootstrap surface only needs to exist because the GTS type
/// registry currently lives inside the `resource-group` gear, which makes
/// a dependent gear's type registration a cross-gear `ClientHub` call
/// during `init` — exactly the phase where `PolicyEnforcer` is
/// structurally unavailable (see above). If the type registry is ever
/// split out into its own gear, this trade-off should be revisited: this
/// bootstrap surface most likely should not survive the split as-is —
/// either it disappears, or it moves with the registry — and type
/// registration should become part of that gear's own contract instead of
/// a carve-out on `resource-group`'s.
///
/// # Error envelope
///
/// Like [`ResourceGroupClient`], every fallible method returns
/// `Result<_, CanonicalError>` per [ADR 0005].
///
/// [ADR 0005]: https://github.com/constructorfabric/gears-rust/blob/main/docs/arch/errors/ADR/0005-cpt-cf-adr-sdk-canonical-projection.md
#[async_trait]
pub trait ResourceGroupTypeBootstrap: Send + Sync {
    /// Get a GTS type definition by its code, without `AuthZ` enforcement.
    /// Mirrors [`ResourceGroupClient::get_type`].
    async fn get_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
    ) -> Result<ResourceGroupType, CanonicalError>;

    /// Create a new GTS type definition, without `AuthZ` enforcement.
    /// Mirrors [`ResourceGroupClient::create_type`].
    async fn create_type(
        &self,
        ctx: &SecurityContext,
        request: CreateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError>;

    /// Update a GTS type definition (full replacement), without `AuthZ`
    /// enforcement. Mirrors [`ResourceGroupClient::update_type`].
    async fn update_type(
        &self,
        ctx: &SecurityContext,
        code: &str,
        request: UpdateTypeRequest,
    ) -> Result<ResourceGroupType, CanonicalError>;
}
