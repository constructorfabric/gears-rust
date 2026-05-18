//! Tenant Metadata SDK contract — wire-shape types only.
//!
//! The SDK ships the JSON-serialisable shapes that cross the
//! [`crate::AccountManagementClient`] trait boundary; validation of
//! the chained `schema_id` string (root-segment check, schema-shape
//! check, GTS-syntax validation) lives **inside** the AM impl crate
//! and surfaces as [`AccountManagementError::InvalidRequest`](crate::AccountManagementError::InvalidRequest) at the boundary.
//! The SDK does not expose a wrapper newtype, the deterministic
//! `UUIDv5` derivation, or the per-schema validation error vocabulary —
//! those are AM internal concerns hidden behind the trait surface.
//!
//! Exposed shapes:
//!
//! * [`MetadataEntry`] — per-tenant metadata projection returned by
//!   reads. `value` is the opaque GTS-validated payload, kept as
//!   [`serde_json::Value`] so downstream consumers do not need a typed
//!   schema dependency. `updated_at` is wire-serialised as RFC 3339.
//! * [`UpsertMetadataRequest`] — request body for the upsert flow.
//!   Validation (`schema_id` chain shape, non-null `value`) happens
//!   server-side; the SDK type is a plain JSON shape.
//! * [`MetadataEntryQuery`] / [`MetadataEntryFilterField`] — `OData`
//!   filter / orderby column declaration used by both the AM repo
//!   layer (`paginate_odata`) and type-safe SDK consumers building
//!   `$filter` predicates.
//!
//! # Why `GtsSchemaId` on the SDK boundary (no AM-local newtype)
//!
//! `schema_id` is typed as the upstream [`gts::GtsSchemaId`] —
//! platform-standard marker for "this string is a GTS schema id" used
//! by sibling SDKs that traffic in chained schema identifiers (e.g.
//! `account_management_sdk::idp::TenantContext::tenant_type`,
//! `account_management_sdk::idp_user::UserTenantContext::tenant_type`,
//! `account_management_sdk::tenant::CreateTenantInput::tenant_type`).
//! Serde sees it as a plain string (the upstream impls forward to
//! `String`-shaped serialize / deserialize), so the wire shape is
//! identical to the old `String`-typed field — only the Rust API
//! gains a type-level discriminator over arbitrary strings.
//!
//! Earlier revisions briefly exposed an AM-local `MetadataSchemaId`
//! newtype paired with a `MetadataValidationError` enum and a
//! `derive_schema_uuid` helper. That surface forced every SDK
//! consumer to learn an AM-specific type and duplicated the `GTS_NS`
//! namespace constant the `gts` crate already owns. The current
//! design moves all validation + UUID derivation inside AM impl
//! (see `crate::domain::metadata::schema_id::ParsedSchemaId`) while
//! the SDK ships only the platform-standard `GtsSchemaId` marker —
//! identical type discipline across modules, zero AM-specific
//! coupling on the contract surface.

use gts::GtsSchemaId;
use modkit_odata_macros::ODataFilterable;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::OffsetDateTime;
use time::serde::rfc3339;
use uuid::Uuid;

/// Public projection of one direct-on-tenant metadata entry.
///
/// Returned by the GET / list endpoints. `schema_id` is the chained
/// `gts.cf.core.am.tenant_metadata.v1~vendor.app.foo.v1~` identifier
/// (always valid because AM impl hydrated it via the types-registry
/// reverse lookup); `value` is the GTS-validated JSON payload;
/// `updated_at` is wire-serialised as RFC 3339.
///
/// `created_at` is intentionally omitted from the public projection:
/// FEATURE §3.1 / §6 only surfaces `updated_at` for cache-validation;
/// keeping the projection minimal avoids leaking row-history details
/// the public contract has not committed to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MetadataEntry {
    /// Chained `gts.cf.core.am.tenant_metadata.v1~vendor.app.foo.v1~`
    /// identifier. Always a valid AM tenant-metadata schema id on the
    /// reads path (server hydrated from the types-registry). Typed
    /// via [`GtsSchemaId`] so the Rust API discriminates it from
    /// arbitrary strings; the JSON wire shape stays a plain string
    /// (upstream serde impls).
    pub schema_id: GtsSchemaId,
    /// Opaque GTS-validated payload.
    pub value: Value,
    /// Last-mutation timestamp.
    #[serde(with = "rfc3339")]
    pub updated_at: OffsetDateTime,
    /// Monotonic version for the optimistic-lock contract. New rows
    /// start at `1`; every successful
    /// [`crate::AccountManagementClient::upsert_metadata`] bumps it
    /// (`current + 1`). Callers MAY pass this value back via
    /// [`UpsertMetadataRequest::expected_version`] to opt into
    /// version-checked writes — a stale version surfaces as
    /// `MetadataVersionMismatch` (HTTP 409). `None`-valued
    /// `expected_version` retains the prior last-write-wins
    /// semantics.
    pub version: i64,
}

impl MetadataEntry {
    /// Build a new [`MetadataEntry`]. The `#[non_exhaustive]` marker
    /// requires consumers to use this constructor (or struct-update
    /// syntax) so future field additions stay SemVer-safe.
    #[must_use]
    pub const fn new(
        schema_id: GtsSchemaId,
        value: Value,
        updated_at: OffsetDateTime,
        version: i64,
    ) -> Self {
        Self {
            schema_id,
            value,
            updated_at,
            version,
        }
    }
}

/// Request shape for `PUT /tenants/{tenant_id}/metadata/{schema_id}`.
///
/// The SDK side does **no** validation: `schema_id` chain-shape is
/// checked by AM impl on entry, and `value` non-null + GTS-schema
/// validation runs there too. Failures surface as
/// [`AccountManagementError::InvalidRequest`](crate::AccountManagementError::InvalidRequest).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct UpsertMetadataRequest {
    /// Chained `gts.cf.core.am.tenant_metadata.v1~vendor.app.foo.v1~`
    /// id. Typed via [`GtsSchemaId`] (platform-standard marker); the
    /// chain-shape / namespace-root validation runs server-side and
    /// surfaces as [`AccountManagementError::InvalidRequest`](crate::AccountManagementError::InvalidRequest) on failure. The
    /// JSON wire shape stays a plain string.
    pub schema_id: GtsSchemaId,
    /// Payload to upsert. Must be non-null and conform to the
    /// registered JSON Schema; both checks run server-side.
    pub value: Value,
    /// Optimistic-lock precondition. **Opt-in** — `None` retains the
    /// historical last-write-wins behaviour; `Some(v)` requires the
    /// stored row's [`MetadataEntry::version`] to equal `v`, and
    /// surfaces [`AccountManagementError::MetadataVersionMismatch`](crate::AccountManagementError::MetadataVersionMismatch)
    /// (HTTP 409) when the stored version drifted between the
    /// caller's read and write.
    ///
    /// Convention for a new entry: the row doesn't exist yet, so the
    /// "current version" is conceptually `0` — passing
    /// `Some(0)` enforces "only create, don't update" semantics, and
    /// `Some(v > 0)` against a missing row surfaces a mismatch
    /// (caller expected an existing row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_version: Option<i64>,
}

impl UpsertMetadataRequest {
    /// Build a request with the two required fields and no version
    /// precondition (`expected_version = None`). Equivalent to the
    /// historical wire shape — last-write-wins.
    #[must_use]
    pub const fn new(schema_id: GtsSchemaId, value: Value) -> Self {
        Self {
            schema_id,
            value,
            expected_version: None,
        }
    }

    /// Builder: attach an optimistic-lock precondition. The write
    /// succeeds only if the stored row's `version` equals `v`; a
    /// drift surfaces as
    /// [`AccountManagementError::MetadataVersionMismatch`](crate::AccountManagementError::MetadataVersionMismatch).
    #[must_use]
    pub const fn with_expected_version(mut self, v: i64) -> Self {
        self.expected_version = Some(v);
        self
    }
}

/// `OData` filter schema for [`crate::AccountManagementClient::list_metadata`].
///
/// Declares the columns of [`MetadataEntry`] that callers may use in
/// `$filter` predicates. The derive expands into the
/// [`MetadataEntryFilterField`] enum the impl-crate's repo layer
/// references when invoking `modkit_db::odata::paginate_odata` —
/// keeping the field set in the SDK keeps the public contract a
/// single source of truth.
///
/// V1 exposes `updated_at` + `schema_uuid` (the deterministic `UUIDv5`
/// derived from `schema_id`, used as the stable cursor tiebreaker).
/// The chained `schema_id` itself is NOT a filter field — exact-schema
/// lookups go through
/// [`crate::AccountManagementClient::get_metadata`] (a single round
/// trip beats `$filter=schema_id eq ...` here, and the chained
/// `schema_id` would otherwise require server-side
/// `schema_id → schema_uuid` rewriting before reaching the SQL layer).
/// Future filterable columns extend this struct in a minor SDK bump.
///
/// The struct is **never** constructed; its only role is to drive the
/// derive macro. The `dead_code` allow keeps clippy quiet on the
/// unused fields — the derive consumes them at compile time.
#[derive(ODataFilterable)]
#[allow(dead_code)]
pub struct MetadataEntryQuery {
    /// Last-mutation timestamp — exposed for cursor pagination and
    /// "recent changes" predicates.
    #[odata(filter(kind = "DateTimeUtc"))]
    pub updated_at: OffsetDateTime,
    /// Deterministic `UUIDv5` derived server-side from the chained
    /// `schema_id` (via the upstream `gts::GtsID::to_uuid()` namespace).
    /// Exposed as a filter / order field so the repo layer can
    /// stable-sort rows whose `updated_at` collide. Callers can also
    /// use it for an exact `$filter=schema_uuid eq <uuid>` lookup
    /// when they already have the derived id in hand; the public
    /// `get_metadata` / `resolve_metadata` paths consume the chained
    /// `schema_id` directly so most consumers will not touch this
    /// field on the filter surface.
    #[odata(filter(kind = "Uuid"))]
    pub schema_uuid: Uuid,
}

pub use MetadataEntryQueryFilterField as MetadataEntryFilterField;

#[cfg(test)]
#[path = "metadata_tests.rs"]
mod tests;
