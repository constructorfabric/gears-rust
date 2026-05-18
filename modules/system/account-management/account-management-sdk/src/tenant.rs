//! Public input/output contract for AM tenant CRUD.
//!
//! These are the shapes that pass through the AM REST boundary
//! (request bodies, query parameters, response envelopes) and through
//! any inter-module Rust callers wired through `ClientHub`. They are
//! deliberately slim and stable:
//!
//! * Inputs ([`CreateTenantRequest`], [`UpdateTenantRequest`]) carry only what
//!   callers must supply. Internal storage details (`UUIDv5` derivations,
//!   hierarchy depth, lifecycle timestamps) live on AM-internal shapes in
//!   the impl crate and never appear here.
//! * Listing input is the platform-wide [`modkit_odata::ODataQuery`]
//!   (`$filter` / `$orderby` / `$top` / `$cursor`); the filterable
//!   column set is declared by [`TenantInfoQuery`] which the
//!   [`#[derive(ODataFilterable)]`](modkit_odata_macros::ODataFilterable)
//!   macro expands into [`TenantInfoFilterField`] for the impl-side
//!   repo. Path-scoped `parent_id` stays a positional argument on
//!   [`crate::AccountManagementClient::list_children`] — it is not a
//!   filter column.
//! * Output is the AM-owned [`Tenant`] shape — a deliberately richer
//!   projection than the resolver's identity-shaped
//!   `tenant_resolver_sdk::TenantInfo`. It carries the lifecycle
//!   timestamps (`created_at`, `updated_at`, `deleted_at`) and the
//!   hierarchy `depth` admin / UI consumers typically want, while
//!   keeping internal-only columns (`retention_window_secs`,
//!   `claimed_by`, `claimed_at`, the raw `tenant_type_uuid`) inside
//!   the impl crate. `deleted_at` doubles as the retention-timer
//!   start; the hard-delete deadline is
//!   `deleted_at + retention_window_secs` (operator override) or the
//!   scanner-default when the override is NULL — both computed
//!   server-side and not surfaced through this shape.
//! * `TenantId` and `TenantStatus` keep being re-used from
//!   `tenant-resolver-sdk` — they are the cross-SDK identity
//!   primitives (the resolver, PEP scopes, closure walks all speak
//!   them) and inlining a parallel copy here would force every
//!   consumer of `Tenant` to translate them back. AM's internal
//!   4-variant status (`Provisioning` + the three public ones) is
//!   service-internal and is filtered out before any value crosses
//!   this boundary.

use gts::GtsSchemaId;
use modkit_odata_macros::ODataFilterable;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::serde::rfc3339;
use uuid::Uuid;

pub use tenant_resolver_sdk::{TenantId, TenantStatus};

/// Public AM tenant projection returned from
/// [`crate::AccountManagementClient`] CRUD and listing surfaces.
///
/// Wider than the resolver's identity-shaped
/// [`tenant_resolver_sdk::TenantInfo`] — `Tenant` is what AM hands
/// to admin tooling, UIs, and inter-module Rust consumers that need
/// to display or reason about a tenant. The extra fields beyond
/// `TenantInfo` are:
///
/// * `depth` — hierarchy depth at write time. `0` for the root.
///   Available so UIs can render tree breadcrumbs without a second
///   round-trip to the closure layer.
/// * `created_at` / `updated_at` / `deleted_at` — lifecycle timestamps.
///   `created_at` is set on insert and immutable thereafter;
///   `updated_at` advances on every mutation. `deleted_at` is
///   `Some(_)` exactly when `status == Deleted` and doubles as the
///   retention-timer start (hard-delete becomes eligible at
///   `deleted_at + retention_window`). All are wire-serialised as
///   RFC 3339.
///
/// Internal-only columns are intentionally **not** here:
/// `tenant_type_uuid` (consumers speak chained `tenant_type` string),
/// `retention_window_secs` (operator override stored per-row but not
/// part of the public lifecycle contract), `claimed_by` / `claimed_at`
/// (hard-delete worker claim). Promoting any of them to the public
/// projection is a `SemVer` minor bump.
///
/// Pre-1.0 SDK: NOT `#[non_exhaustive]`. Listing all fields in a
/// struct expression is the documented construction style — the
/// impl-side service composes `Tenant` directly without going
/// through a constructor, and adding a column is an explicit `SemVer`
/// minor bump that requires updating every literal site (the
/// compiler will flag them via the `missing field` error). Promoting
/// to `#[non_exhaustive]` is a 1.0 / wider-adoption decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "`tenant_type` mirrors the field name on `tenant_resolver_sdk::TenantInfo` and on the storage entity; renaming for clippy would diverge from cross-SDK vocabulary"
)]
pub struct Tenant {
    pub id: TenantId,
    pub name: String,
    pub status: TenantStatus,
    /// Chained `gts.cf.core.am.tenant_type.v1~vendor.app.foo.v1~`
    /// identifier of the registered type. `None` when the types
    /// registry was momentarily unreachable at the lowering site;
    /// AM does not block read responses on a registry blip, so this
    /// field is best-effort. Mirrors the same `Option<String>` policy
    /// `tenant_resolver_sdk::TenantInfo` uses for cross-SDK
    /// consistency.
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub tenant_type: Option<String>,
    /// Parent tenant id. `None` for the platform root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<TenantId>,
    /// `true` when this tenant is the start of a self-managed
    /// subtree. Ancestors cannot traverse into it under the default
    /// (non barrier-penetrating) scope.
    #[serde(default)]
    pub self_managed: bool,
    /// Hierarchy depth recorded at insert. `0` for the root, `parent.depth + 1`
    /// for every child.
    pub depth: u32,
    #[serde(with = "rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Soft-delete tombstone. `Some(_)` exactly when
    /// `status == Deleted`; also marks the start of the retention
    /// timer (hard-delete becomes eligible at
    /// `deleted_at + retention_window`).
    #[serde(
        default,
        with = "rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub deleted_at: Option<OffsetDateTime>,
}

/// Input for the create-child-tenant flow.
///
/// `tenant_type` is a chained GTS identifier (e.g.
/// `gts.cf.core.am.tenant_type.v1~cf.core.am.customer.v1~`); AM derives
/// the canonical `UUIDv5` via [`gts::GtsID`] internally so callers do not
/// supply two parallel identifiers (which used to drift). The field is
/// typed [`GtsSchemaId`] rather than `String` so callers (REST handler,
/// inter-module Rust consumers) get a self-documenting contract and
/// any generated JSON Schema annotates the field with
/// `format: gts-schema-id`. Wire shape stays a string.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CreateTenantRequest {
    pub child_id: Uuid,
    pub parent_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub self_managed: bool,
    pub tenant_type: GtsSchemaId,
    /// Opaque provider-specific metadata forwarded to the `IdP` plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provisioning_metadata: Option<Value>,
}

impl CreateTenantRequest {
    /// Construct the input with the four required fields.
    /// `self_managed` defaults to `false`; `provisioning_metadata`
    /// defaults to `None`. Use [`Self::with_self_managed`] and
    /// [`Self::with_provisioning_metadata`] when overriding.
    #[must_use]
    pub fn new(
        child_id: Uuid,
        parent_id: Uuid,
        name: impl Into<String>,
        tenant_type: GtsSchemaId,
    ) -> Self {
        Self {
            child_id,
            parent_id,
            name: name.into(),
            self_managed: false,
            tenant_type,
            provisioning_metadata: None,
        }
    }

    #[must_use]
    pub const fn with_self_managed(mut self, self_managed: bool) -> Self {
        self.self_managed = self_managed;
        self
    }

    #[must_use]
    pub fn with_provisioning_metadata(mut self, value: Value) -> Self {
        self.provisioning_metadata = Some(value);
        self
    }
}

/// Patch shape for the update-tenant flow. Carries only **mutable**
/// tenant fields; an empty patch (all fields `None`) is rejected at
/// the service boundary.
///
/// `status` is **not** part of this shape — lifecycle transitions go
/// through dedicated methods:
/// [`crate::AccountManagementClient::suspend_tenant`] /
/// [`crate::AccountManagementClient::unsuspend_tenant`] for the
/// `Active` ↔ `Suspended` flip and
/// [`crate::AccountManagementClient::delete_tenant`] for soft-delete.
/// This keeps each lifecycle transition idempotent on its own surface
/// and avoids exposing the AM-internal status taxonomy as a patchable
/// field.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UpdateTenantRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl UpdateTenantRequest {
    /// Construct an empty patch. Fields default to `None` and are set
    /// via [`Self::with_name`]. An all-`None` patch is rejected at the
    /// service boundary.
    #[must_use]
    pub const fn new() -> Self {
        Self { name: None }
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Whether this patch is effectively empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.name.is_none()
    }
}

/// `OData` filter schema for
/// [`crate::AccountManagementClient::list_children`].
///
/// Declares the columns of [`TenantInfo`] (and the storage-side
/// `tenants` row) that callers may use in `$filter` and `$orderby`
/// predicates. The derive expands into the [`TenantInfoFilterField`]
/// enum the impl-crate's repo layer references when invoking
/// `modkit_db::odata::paginate_odata`; keeping the field set in the
/// SDK keeps the public contract a single source of truth.
///
/// Path-scoped `parent_id` is NOT part of this surface — it stays a
/// positional argument on `list_children`. Listing for an arbitrary
/// `parent_id` via `$filter` would let callers bypass the
/// parent-existence + status guard the service layer runs before the
/// repo call.
///
/// `id` is exposed primarily as the **cursor tiebreaker** — the
/// listing surface composes `(created_at ASC, id ASC)` as its
/// effective sort so two siblings sharing a `created_at` timestamp
/// (microsecond collisions on batch INSERTs or parallel API calls)
/// stay disambiguated across page boundaries. Without a unique
/// secondary key the cursor predicate `created_at > last_ts`
/// silently skips collision-mates on the next page. Exact-id lookups
/// still go through `get_tenant` — the `OData` filter surface accepts
/// `$filter=id eq <uuid>` for niche bulk-export flows but most
/// callers will use the dedicated endpoint.
///
/// Status filtering retains the AM-default hidden-AND: when a caller
/// supplies no `status` predicate, the repo layer ANDs
/// `status IN (Active, Suspended)` so soft-deleted rows stay hidden by
/// default. Callers wanting to see deleted rows opt in via
/// `$filter=status eq 'deleted'`. The accepted string values match the
/// serde rename on the public [`TenantStatus`] enum (`"active"`,
/// `"suspended"`, `"deleted"`); the AM-internal `Provisioning` status
/// has no SDK representation and is rejected as a validation error.
///
/// The struct is **never** constructed; its only role is to drive the
/// derive macro. The `dead_code` allow keeps clippy quiet on the
/// unused fields — the derive consumes them at compile time.
#[derive(ODataFilterable)]
#[allow(dead_code)]
pub struct TenantInfoQuery {
    /// `tenants.id` (UUID, primary key). Exposed as the cursor
    /// tiebreaker so `list_children` can compose
    /// `(created_at ASC, id ASC)` as a total order — preventing
    /// silent row-loss when sibling tenants share a `created_at`
    /// timestamp. Filter use is allowed (`$filter=id eq <uuid>`)
    /// but the dedicated `get_tenant` endpoint is the ergonomic
    /// path for exact-id reads.
    #[odata(filter(kind = "Uuid"))]
    pub id: Uuid,
    /// `tenants.status` projected as the public [`TenantStatus`]
    /// lifecycle string: `"active"`, `"suspended"`, or `"deleted"`
    /// (the serde rename on the SDK enum). The `OData` parser
    /// validates the string up-front; unknown values — including the
    /// AM-internal `"provisioning"` — are rejected as a validation
    /// error before the predicate reaches storage.
    #[odata(filter(kind = "String"))]
    pub status: String,
    /// Deterministic `UUIDv5` of the registered tenant-type schema id.
    /// Filtering on the UUID rather than the chained `gts.*` string
    /// keeps the wire path simple — callers building a UI dropdown
    /// over tenant types already hold the UUID (the resolver registry
    /// returns it on type lookups), and the SDK does not need a
    /// server-side `derive_schema_uuid` rewrite step. Exact-type
    /// listings go through `$filter=tenant_type_uuid eq <uuid>`.
    #[odata(filter(kind = "Uuid"))]
    pub tenant_type_uuid: Uuid,
    /// `tenants.self_managed` flag. Useful to surface "boundary"
    /// child tenants vs ordinary ones in operator UIs.
    #[odata(filter(kind = "Bool"))]
    pub self_managed: bool,
    /// Row creation timestamp. Exposed as a filter / order column so
    /// callers can paginate chronologically. When callers omit
    /// `$orderby`, the impl-side `list_children` injects
    /// `created_at ASC` so the chronological default is preserved;
    /// `id ASC` (declared above) is then appended as the unique
    /// tiebreaker, yielding effective order `(created_at ASC, id ASC)`.
    #[odata(filter(kind = "DateTimeUtc"))]
    pub created_at: OffsetDateTime,
    /// Last-mutation timestamp. Available for `$filter` /
    /// `$orderby` so callers can watch for recently-changed tenants
    /// (e.g. a UI sidebar "recent activity" panel).
    #[odata(filter(kind = "DateTimeUtc"))]
    pub updated_at: OffsetDateTime,
}

pub use TenantInfoQueryFilterField as TenantInfoFilterField;

#[cfg(test)]
#[path = "tenant_tests.rs"]
mod tests;
