//! Public inter-module client trait for the Account Management module.
//!
//! [`AccountManagementClient`] is the single seam other modules /
//! plugins / the AM REST handler call into to drive AM's tenant and
//! user surfaces. Consumers obtain it from `ClientHub`:
//!
//! ```ignore
//! let client = ctx.client_hub().get::<dyn AccountManagementClient>()?;
//! let tenant = client.get_tenant(&sec_ctx, tenant_id).await?;
//! ```
//!
//! # Uniform `SecurityContext` posture
//!
//! Every public method on this trait takes a [`SecurityContext`]; the
//! impl-side service layer compiles the caller-supplied context into
//! an [`AccessScope`] via `PolicyEnforcer::access_scope_with` and
//! forwards that scope through the storage seam. Callers never
//! construct an `AccessScope` directly. This is the same posture
//! [`resource_group_sdk::ResourceGroupClient`] uses on every method;
//! both SDKs avoid leaking PDP-output shapes through their public
//! contract.
//!
//! # Conversion surface is intentionally absent
//!
//! `request_conversion` / `cancel` / `reject` / `approve` /
//! `list_inbound_for_parent` / `list_own_for_tenant` live on
//! `ConversionService` in the impl crate but are not yet plumbed
//! through this trait — their DTOs
//! (`ConversionRequest`, `ConversionCaller`, `ConversionStatus`,
//! `TargetMode`, `ListConversionsQuery`, …) have not been hoisted
//! into the SDK yet, and inter-module Rust consumers of the
//! conversion lifecycle do not exist outside the (future) REST
//! handler. They will join `AccountManagementClient` once the
//! conversion DTO move lands. Until then the REST handler depends on
//! the impl crate directly for conversion endpoints.
//!
//! # Tenant metadata surface
//!
//! Five PEP-gated methods (`get_metadata`, `resolve_metadata`,
//! `list_metadata`, `upsert_metadata`, `delete_metadata`) drive the AM
//! tenant-metadata feature. The listing surface follows the
//! `resource-group-sdk` convention — callers supply a
//! [`modkit_odata::ODataQuery`] (filter / order / cursor / select)
//! and AM returns a [`modkit_odata::Page<MetadataEntry>`]. The cursor
//! is AM-owned ([`modkit_odata::CursorV1`]) and the wire envelope is
//! identical to the one `resource-group-sdk::list_groups` /
//! `list_types` already use.
//!
//! `get_metadata` and `resolve_metadata` are deliberately separate
//! operations: `get_metadata` reads the row attached to a single
//! tenant (404 on miss), while `resolve_metadata` walks up the
//! ancestor chain honouring `self_managed` barriers and
//! `OverrideOnly` policy (returns `None` on a legitimate miss).

use async_trait::async_trait;
use gts::GtsSchemaId;
use modkit_odata::{ODataQuery, Page};
use modkit_security::SecurityContext;
use uuid::Uuid;

use crate::error::AccountManagementError;
use crate::idp_user::{IdpNewUser, IdpUser, ListUsersQuery};
use crate::metadata::{MetadataEntry, UpsertMetadataRequest};
use crate::tenant::{CreateTenantRequest, Tenant, UpdateTenantRequest};

/// Public inter-module client trait for the Account Management module.
///
/// See the module docstring for the uniform `SecurityContext`
/// posture and the deferred-surface notes (conversion).
///
/// # Error envelope
///
/// All methods return [`AccountManagementError`] — a flat enum
/// mirroring the AIP-193 categories AM raises. AM's impl crate maps
/// `DomainError → AccountManagementError` at this boundary
/// (`infra::sdk_error_mapping::From<DomainError> for AccountManagementError`);
/// the REST handler (when it lands) lifts to
/// `modkit_canonical_errors::CanonicalError` via the
/// `account_management_error_to_canonical` helper in the same module.
/// Consumers match on [`AccountManagementError`] variants directly
/// and never depend on `modkit-canonical-errors`.
#[async_trait]
pub trait AccountManagementClient: Send + Sync + 'static {
    // -----------------------------------------------------------------
    // Tenant CRUD — PEP-gated inside the impl by `TenantService::authorize`
    // -----------------------------------------------------------------

    /// Create a child tenant under `input.parent_id`. Implements the
    /// three-step create-tenant saga (insert provisioning row -> `IdP`
    /// `provision_tenant` -> activation finalisation) with full
    /// compensation on each branch failure.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` (HTTP 403) -- caller is not authorized to
    ///   create a tenant under `parent_id`.
    /// * `InvalidArgument` (HTTP 400) -- parent missing / inactive,
    ///   target type incompatible, depth ceiling exceeded.
    /// * `Unimplemented` (HTTP 501) -- the configured `IdP` plugin
    ///   reports the operation as unsupported.
    /// * `ServiceUnavailable` (HTTP 503) -- `IdP` transport failure;
    ///   the saga compensates the local provisioning row.
    async fn create_tenant(
        &self,
        ctx: &SecurityContext,
        input: CreateTenantRequest,
    ) -> Result<Tenant, AccountManagementError>;

    /// Read a tenant by id. Returns the live tenant row projected to
    /// [`Tenant`]. AM-internal `Provisioning` rows surface as
    /// `NotFound` (they are not part of the SDK contract). Soft-deleted
    /// tenants **are** returned — with `status == Deleted` and
    /// `deleted_at` set to the soft-delete timestamp — so callers can
    /// observe the tombstone during the retention window. Callers that
    /// want to ignore soft-deleted rows match on `status` themselves.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` (HTTP 403) -- caller has no scope to read
    ///   this tenant.
    /// * `NotFound` (HTTP 404) -- tenant does not exist, is
    ///   `Provisioning`, or sits outside the caller's PDP-compiled
    ///   subtree.
    async fn get_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, AccountManagementError>;

    /// List direct children of `parent_id`. The listing is
    /// **direct-only** (`WHERE tenants.parent_id = :parent_id`) — full
    /// subtree walks go through the tenant-closure layer, not this
    /// surface. The PDP-compiled scope clamps the result to the
    /// caller's subtree; self-managed children sitting behind a
    /// closure barrier (`barrier = 1`) are excluded unless the caller
    /// explicitly holds a barrier-penetrating scope.
    ///
    /// Filtering / ordering / cursor pagination go through `query`
    /// (`$filter`, `$orderby`, `$top`, `$cursor`). The filterable
    /// column set is declared by [`crate::TenantInfoQuery`]; callers
    /// may filter on `status`, `tenant_type_uuid`, `self_managed`,
    /// `created_at`, `updated_at`. Path-scoped `parent_id` is NOT a
    /// filter column; the listing surface is always scoped to a
    /// single parent.
    ///
    /// When `query` does not mention `status`, the repo layer ANDs
    /// `status IN (Active, Suspended)` so soft-deleted rows stay
    /// hidden by default. Callers wanting to see deleted rows pass
    /// `$filter=status eq 'deleted'` explicitly (string form matching
    /// the [`crate::TenantStatus`] serde rename).
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` (HTTP 403) / `NotFound` (HTTP 404) -- see
    ///   [`Self::get_tenant`].
    /// * `InvalidArgument` (HTTP 400) -- `$filter` / `$orderby`
    ///   references a column not declared in
    ///   [`crate::TenantInfoQuery`], or cursor token rejected by the
    ///   [`modkit_odata::CursorV1`] validator (filter-hash mismatch,
    ///   truncated token, order-mismatch).
    async fn list_children(
        &self,
        ctx: &SecurityContext,
        parent_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<Tenant>, AccountManagementError>;

    /// PATCH-style update on the mutable tenant fields. An empty
    /// patch is rejected as `InvalidArgument`.
    ///
    /// `status` is **not** mutable through this method — use
    /// [`Self::suspend_tenant`] / [`Self::unsuspend_tenant`] for the
    /// `Active` ↔ `Suspended` flip and [`Self::delete_tenant`] for
    /// soft-delete. Calling `update_tenant` on an already-deleted
    /// tenant surfaces as `FailedPrecondition` (the row is read-only
    /// during the retention window).
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` / `NotFound` -- see [`Self::get_tenant`].
    /// * `InvalidArgument` (HTTP 400) -- empty patch, name validation
    ///   failure, GTS schema rejection on a patched field.
    /// * `FailedPrecondition` (HTTP 400) -- target tenant is in
    ///   `Deleted` status (read-only during retention).
    async fn update_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
        patch: UpdateTenantRequest,
    ) -> Result<Tenant, AccountManagementError>;

    /// Transition `id` from `Active` to `Suspended`. Calling on an
    /// already-`Suspended` tenant is an idempotent no-op and returns
    /// the same tenant projection. `Deleted` tenants are rejected as
    /// `FailedPrecondition` (lifecycle is terminal during retention).
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` / `NotFound` -- see [`Self::get_tenant`].
    /// * `FailedPrecondition` (HTTP 400) -- target tenant is in
    ///   `Deleted` status.
    async fn suspend_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, AccountManagementError>;

    /// Transition `id` from `Suspended` back to `Active`. Calling on
    /// an already-`Active` tenant is an idempotent no-op. `Deleted`
    /// tenants are rejected as `FailedPrecondition`.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` / `NotFound` -- see [`Self::get_tenant`].
    /// * `FailedPrecondition` (HTTP 400) -- target tenant is in
    ///   `Deleted` status.
    async fn unsuspend_tenant(
        &self,
        ctx: &SecurityContext,
        id: Uuid,
    ) -> Result<Tenant, AccountManagementError>;

    /// Schedule a soft-delete of `tenant_id`. The tenant transitions
    /// to `Deleted` with `deleted_at` stamped to the current wall-clock
    /// (which also starts the retention timer — the sweep runs at
    /// `deleted_at + retention_window`); the retention sweep performs
    /// the `IdP`-side deprovision + hard-delete asynchronously.
    ///
    /// **Idempotent**. Calling on a tenant that is already in
    /// `Deleted` status returns the existing tombstone (with the
    /// original `deleted_at`) without re-dispatching the preflight
    /// checks, the resource-group probe, or the DB write — the
    /// retention timer is not restarted.
    ///
    /// Refuses the root tenant, tenants with non-deleted children,
    /// and tenants with active resource-group memberships.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` / `NotFound` -- see [`Self::get_tenant`].
    /// * `FailedPrecondition` (HTTP 400) -- tenant has children OR
    ///   active resource references OR is the platform root.
    async fn delete_tenant(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
    ) -> Result<Tenant, AccountManagementError>;

    // -----------------------------------------------------------------
    // IdpUser CRUD — PEP-gated inside the impl by `UserService::authorize`
    // -----------------------------------------------------------------

    /// Provision a user inside `tenant_id` via the configured `IdP`
    /// plugin. The plugin owns the user-side identity primitives;
    /// AM only orchestrates the saga (tenant guard -> `IdP` plugin
    /// call -> projection back to [`IdpUser`]).
    ///
    /// The actor recorded on audit lines is `ctx.subject_id()`.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` (HTTP 403) -- caller is not authorised to
    ///   create users on `tenant_id`.
    /// * `NotFound` (HTTP 404) -- `tenant_id` does not resolve, is
    ///   soft-deleted, or sits outside the caller's PDP-compiled
    ///   subtree.
    /// * `InvalidArgument` (HTTP 400) -- payload validation failure
    ///   (username trim / max length, profile field length, etc.) OR
    ///   the `IdP` plugin rejected the request.
    /// * `Unimplemented` (HTTP 501) -- `IdP` plugin declined the
    ///   operation.
    /// * `ServiceUnavailable` (HTTP 503) -- `IdP` transport failure.
    async fn create_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        payload: IdpNewUser,
    ) -> Result<IdpUser, AccountManagementError>;

    /// Fetch a single user by id from `tenant_id` via the configured
    /// `IdP` plugin. Thin wrapper over [`Self::list_users`] with
    /// `user_id_filter = Some(user_id)`, `top = 1`, `cursor = None`.
    /// Returns `NotFound` when the user is not present in the
    /// plugin's response (empty page is success on `list_users`; the
    /// `get_user` shape collapses it to `NotFound` for REST semantics
    /// — `GET /users/{id}` is either 200 or 404).
    ///
    /// Profile-mutation (`email` / `display_name` / `username`) is
    /// intentionally not exposed by AM: those attributes live in the
    /// `IdP` and `SCIM`-style edits go directly to the provider's admin
    /// API per `cpt-cf-account-management-adr-idp-user-identity-source-of-truth`.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` / `NotFound` -- as on [`Self::create_user`].
    /// * All other categories surfaced by [`Self::list_users`].
    async fn get_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<IdpUser, AccountManagementError>;

    /// List users in `tenant_id` via the configured `IdP` plugin.
    /// `user_id_filter = Some(_)` is the authoritative existence
    /// signal consumed by sibling features (e.g. user-groups
    /// membership writes): the empty page is success-with-absence,
    /// NOT `NotFound`.
    ///
    /// When `user_id_filter` is set the AM seam enforces
    /// `pagination.top == 1` and `pagination.cursor == None` so the
    /// filtered lookup keeps single-row existence semantics; either
    /// violation surfaces as `InvalidArgument`.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` / `NotFound` -- as on
    ///   [`Self::create_user`].
    /// * `InvalidArgument` -- tenant is not `Active`, pagination
    ///   contract violated, or `IdP` plugin rejected the request.
    /// * `Unimplemented` / `ServiceUnavailable` -- as on
    ///   [`Self::create_user`].
    async fn list_users(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: ListUsersQuery,
    ) -> Result<Page<IdpUser>, AccountManagementError>;

    /// Deprovision a user from `tenant_id` via the configured `IdP`
    /// plugin. AM's deprovision saga additionally cleans up
    /// dangling user-group memberships before delegating to the `IdP`
    /// plugin teardown. Vendor-side "user already gone" responses are
    /// mapped to success by the plugin layer (idempotent contract).
    ///
    /// The actor recorded on audit lines is `ctx.subject_id()`.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` / `NotFound` -- as on
    ///   [`Self::create_user`].
    /// * `InvalidArgument` -- tenant inactive.
    /// * `ServiceUnavailable` / `Unimplemented` -- as on
    ///   [`Self::create_user`].
    async fn delete_user(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        user_id: Uuid,
    ) -> Result<(), AccountManagementError>;

    // -----------------------------------------------------------------
    // Tenant metadata — PEP-gated inside the impl by `MetadataService::authorize`
    // -----------------------------------------------------------------

    /// Read the metadata entry attached **directly** to `tenant_id`
    /// for `schema_id`. Does NOT walk up the ancestor chain — use
    /// [`Self::resolve_metadata`] for the inheritance-aware lookup.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied` (HTTP 403) — caller has no scope to read
    ///   metadata on this tenant.
    /// * `NotFound` (HTTP 404) — tenant does not exist OR no row
    ///   exists at `(tenant_id, schema_id)`. The two cases carry
    ///   distinct AM resource types
    ///   (`gts.cf.core.am.tenant.v1~` vs
    ///   `gts.cf.core.am.tenant_metadata.v1~`) on the canonical
    ///   envelope so consumers can disambiguate.
    /// * `FailedPrecondition` (HTTP 400) — tenant is not `Active`.
    /// * `ServiceUnavailable` (HTTP 503) — types-registry transport
    ///   failure on the schema-existence gate.
    async fn get_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: GtsSchemaId,
    ) -> Result<MetadataEntry, AccountManagementError>;

    /// Resolve the **effective** metadata for `tenant_id` at
    /// `schema_id`, walking up the ancestor chain per the FEATURE
    /// algorithm:
    ///
    /// 1. Direct row at `tenant_id` — hit returns immediately.
    /// 2. `OverrideOnly` schema policy — short-circuits with `None`.
    /// 3. `self_managed` start tenant — short-circuits with `None`
    ///    (start-tenant barrier).
    /// 4. Walk up ancestors; `self_managed` ancestor terminates the
    ///    walk; suspended ancestors are skipped without reading.
    ///
    /// `Ok(None)` is a legitimate "no value anywhere in the chain"
    /// success, distinct from the `NotFound` raised by
    /// [`Self::get_metadata`].
    ///
    /// # Errors
    ///
    /// * `PermissionDenied`, `FailedPrecondition`, `ServiceUnavailable`
    ///   as on [`Self::get_metadata`].
    /// * `NotFound` (HTTP 404) — tenant does not exist OR schema is
    ///   not registered.
    async fn resolve_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: GtsSchemaId,
    ) -> Result<Option<MetadataEntry>, AccountManagementError>;

    /// List metadata entries attached directly to `tenant_id`,
    /// filtered + paginated via the supplied [`ODataQuery`]. The
    /// query supports `$filter`, `$orderby`, `$top` and `$cursor`
    /// over `MetadataEntry` columns (`schema_id`, `updated_at`).
    /// Inherited entries are NOT included — this is a direct-only
    /// listing (mirrors [`Self::get_metadata`] vs [`Self::resolve_metadata`]).
    ///
    /// # Errors
    ///
    /// * `PermissionDenied`, `FailedPrecondition` as on
    ///   [`Self::get_metadata`].
    /// * `NotFound` (HTTP 404) — tenant does not exist.
    /// * `InvalidArgument` (HTTP 400) — cursor token rejected by the
    ///   [`modkit_odata::CursorV1`] validator (filter-hash mismatch,
    ///   truncated token, order-mismatch).
    async fn list_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<MetadataEntry>, AccountManagementError>;

    /// Upsert the metadata row at `(tenant_id, input.schema_id)`.
    /// Returns the post-write [`MetadataEntry`] — REST handlers
    /// in front of this method MAY emit HTTP 200 uniformly per
    /// RFC 7231 PUT semantics, OR distinguish 201/200 by GET-first;
    /// the SDK contract does NOT surface the insert-vs-update
    /// discriminator (per-`am.events` audit logging inside the
    /// service preserves the distinction for aggregators that
    /// need it).
    ///
    /// The actor recorded on audit lines is `ctx.subject_id()`.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied`, `FailedPrecondition` as on
    ///   [`Self::get_metadata`].
    /// * `NotFound` (HTTP 404) — tenant does not exist OR schema is
    ///   not registered.
    /// * `InvalidArgument` (HTTP 400) — `input.value` violates the
    ///   registered JSON Schema.
    /// * `ServiceUnavailable` (HTTP 503) — types-registry transport
    ///   failure.
    /// * `Internal` (HTTP 500) — registered schema is not a valid
    ///   JSON Schema (catalog drift).
    async fn upsert_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        input: UpsertMetadataRequest,
    ) -> Result<MetadataEntry, AccountManagementError>;

    /// Delete the metadata row attached **directly** to `tenant_id`
    /// for `schema_id`. Inherited entries (resolved through an
    /// ancestor) are NOT affected — only the direct row is removed.
    ///
    /// The actor recorded on audit lines is `ctx.subject_id()`.
    ///
    /// # Errors
    ///
    /// * `PermissionDenied`, `FailedPrecondition` as on
    ///   [`Self::get_metadata`].
    /// * `NotFound` (HTTP 404) — tenant does not exist, schema is
    ///   not registered, OR no direct row exists at
    ///   `(tenant_id, schema_id)`.
    async fn delete_metadata(
        &self,
        ctx: &SecurityContext,
        tenant_id: Uuid,
        schema_id: GtsSchemaId,
    ) -> Result<(), AccountManagementError>;
}
