//! `MetadataRepo` — storage seam for the `tenant_metadata` table.
//!
//! [`MetadataRepo`] is the sole storage seam the metadata domain layer
//! touches. It abstracts the `SeaORM`-backed implementation that lands
//! in Phase 4 (`crate::infra::storage::repo_impl::metadata`) so the
//! future `MetadataService` can be unit-tested against a pure in-memory
//! fake (`crate::domain::metadata::test_support::FakeMetadataRepo`).
//!
//! Trait-method shape notes:
//!
//! * Every method owns its own short-lived transaction (the entity is
//!   the leaf write — no saga composition is required because metadata
//!   does not co-mutate `tenants` / `tenant_closure`).
//! * `list_for_tenant` pushes pagination into the SQL `LIMIT`/`OFFSET`
//!   plus a separate `COUNT(*)`, and returns [`MetadataRowsPage`]
//!   (rows + total) so the service layer can build the public list
//!   envelope without re-counting in memory.
//! * `upsert_for_tenant` returns an [`UpsertOutcome`] discriminator the
//!   service layer maps onto HTTP 201 (insert) / HTTP 200 (update) per
//!   FEATURE §6 AC line 393. The `now` parameter follows the AM
//!   convention of injecting the wall-clock at the service boundary so
//!   unit tests can pin it.
//! * `delete_for_tenant` is non-idempotent on missing rows: the
//!   distinct-404 contract (FEATURE §6 AC line 394) requires
//!   [`DomainError::MetadataEntryNotFound`] when no `(tenant_id,
//!   schema_uuid)` row exists.

use async_trait::async_trait;
use modkit_odata::{ODataQuery, Page};
use modkit_security::AccessScope;
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::metadata::{MetadataRow, UpsertOutcome};

/// Read / write boundary for the `tenant_metadata` table.
///
/// Every method owns its own short-lived transaction unless the method
/// docs state otherwise. Caller-facing methods accept an [`AccessScope`]
/// parameter that the implementation forwards to `modkit_db`'s secure
/// query builders.
///
/// # Caller contract on `scope`
///
/// The `tenant_metadata` entity is declared `Scopable(tenant_col =
/// "tenant_id", no_resource, no_owner, no_type)`, so a caller-built
/// [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
/// scope (cyberware-rust#1813) clamps every method on this trait to
/// the caller's tenant subtree via the secure-ORM closure subquery
/// (`tenant_id IN (SELECT descendant FROM tenant_closure WHERE
/// ancestor = :root AND barrier = 0 ...)`). The trait simply forwards
/// the caller's [`AccessScope`] through `modkit_db`'s secure builders
/// — the REST handler is responsible for constructing the scope.
///
/// System-actor callers (cascade cleanup on tenant hard-delete; future
/// retention sweeps) pass [`AccessScope::allow_all`] explicitly; that
/// posture is owned by `TenantRepoImpl::hard_delete_one`, not by this
/// trait.
///
/// # Cascade-delete on tenant removal
///
/// Per FEATURE §1.2 / `DoD` `dod-tenant-metadata-cascade-delete`, all
/// metadata rows for a tenant MUST be removed atomically with the
/// tenant row. The mechanism is owned by `TenantRepoImpl::hard_delete_one`
/// — that path issues a single in-TX `delete_many` against
/// `tenant_metadata` for dialect portability (works on both PG and
/// `SQLite`, regardless of whether `PRAGMA foreign_keys` is enabled). The
/// metadata trait deliberately exposes no cascade-cleanup method
/// because the lifecycle TX owns the order of `tenant_closure` →
/// `tenant_metadata` → `tenants` deletions; routing through this trait
/// would require an extra `MetadataRepo` plumb-through on
/// `TenantRepoImpl` for no behavioural gain.
#[async_trait]
pub trait MetadataRepo: Send + Sync {
    // ---- Reads ---------------------------------------------------------

    /// List the tenant's direct entries, paginated via the supplied
    /// [`ODataQuery`] (filter + order + cursor + limit). Stable
    /// tiebreaker on `schema_uuid` keeps cursor re-reads deterministic.
    /// Inherited values from ancestors are NOT walked here — listing
    /// is per-tenant only per FEATURE §3.1 (the `/resolved` endpoint
    /// owns inheritance and lives on the service layer).
    ///
    /// Returns a [`modkit_odata::Page<MetadataRow>`] whose `page_info`
    /// carries the next-cursor token the service forwards on. The
    /// caller (service layer) re-hydrates the chained `schema_id` for
    /// each row before exposing the result.
    async fn list_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        query: &ODataQuery,
    ) -> Result<Page<MetadataRow>, DomainError>;

    /// Load a single direct entry by `(tenant_id, schema_uuid)`.
    ///
    /// Returns `Ok(None)` when no row exists or the row is outside the
    /// supplied `scope`. The service layer (Phase 3) translates `None`
    /// into [`DomainError::MetadataEntryNotFound`] for the per-schema
    /// GET endpoint; the repo never raises that error itself because
    /// the walk-up resolver also calls this method and treats the
    /// `None` arm as a non-error continuation signal.
    async fn get_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        schema_uuid: Uuid,
    ) -> Result<Option<MetadataRow>, DomainError>;

    // ---- Writes --------------------------------------------------------

    /// Upsert the row at `(tenant_id, schema_uuid)`.
    ///
    /// The `value` payload is opaque to the repo — the service layer
    /// (Phase 3) is responsible for GTS schema validation BEFORE
    /// invoking this method. Insert paths stamp `created_at = now AND
    /// updated_at = now`; update paths preserve the original
    /// `created_at` and stamp `updated_at = now`.
    ///
    /// Returns [`UpsertOutcome::Inserted`] when the row did not exist
    /// before this call and [`UpsertOutcome::Updated`] when an existing
    /// row was rewritten. The caller (service layer in Phase 3) maps
    /// the discriminator onto HTTP 201 / HTTP 200 per FEATURE §6 AC
    /// line 393.
    async fn upsert_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        schema_uuid: Uuid,
        value: Value,
        now: OffsetDateTime,
        expected_version: Option<i64>,
    ) -> Result<UpsertOutcome, DomainError>;

    /// Delete the row at `(tenant_id, schema_uuid)`.
    ///
    /// # Errors
    ///
    /// * [`DomainError::MetadataEntryNotFound`] — no row exists at
    ///   `(tenant_id, schema_uuid)`. DELETE is intentionally NOT
    ///   idempotent-success on missing rows because the distinct-404
    ///   contract (FEATURE §6 AC line 394) makes the signal observable
    ///   to clients.
    async fn delete_for_tenant(
        &self,
        scope: &AccessScope,
        tenant_id: Uuid,
        schema_uuid: Uuid,
    ) -> Result<(), DomainError>;
}
