//! `SeaORM` entity for the AM-owned `tenants` table.
//!
//! Mirrors the `tenants` schema declared by `m0001_initial_schema`
//! column-for-column and matches DESIGN §3.7. The `status` and `depth`
//! fields are stored as `SMALLINT` / `INTEGER` at the DB level but
//! surfaced through the domain layer via the
//! [`crate::domain::tenant::model::TenantStatus`] enum and a `u32` depth.
//!
//! Repository implementation and domain-to-entity mapping live in
//! `infra/storage/repo_impl/`.

use modkit_db_macros::Scopable;
use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use uuid::Uuid;

// @cpt-begin:cpt-cf-account-management-dbtable-tenants:p1:inst-dbtable-tenants-entity
// `tenants` is declared `no_tenant, resource_col = "id", no_owner,
// no_type` — DB-level subtree clamp engages on the row's own primary
// key via the
// [`InTenantSubtree`](modkit_security::ScopeFilter::in_tenant_subtree)
// predicate (cyberware-rust#1813). Compiles to
// `tenants.id IN (SELECT descendant_id FROM tenant_closure
//   WHERE ancestor_id = :root_tenant_id AND barrier = 0)`.
//
// Why `resource_col = "id"` and not `tenant_col = "id"`:
// `pep_properties::RESOURCE_ID` is the semantically clean choice
// because subtree-membership questions on `tenants` are
// identity-based ("the row about which the request is made"), not
// ownership-based. `OWNER_TENANT_ID` would be circular here (a
// tenant's owner tenant is itself); the convention also matches the
// existing `InGroupSubtree` stack where `resource_groups` uses
// `resource_col = "id"`.
//
// Authorization end-to-end is now defence-in-depth: the PDP gate at
// the service layer (DESIGN §4.2) authorizes the caller, and the
// compiled `AccessScope` is forwarded into the repo so the
// secure-extension layer materialises the subtree-clamp JOIN
// against `tenant_closure`. A caller scoped to T1 reading T2 outside
// T1's subtree collapses to `NotFound` at the database, not at a
// hand-rolled in-Rust check.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Scopable)]
#[sea_orm(table_name = "tenants")]
#[secure(no_tenant, resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    #[sea_orm(nullable)]
    pub parent_id: Option<Uuid>,
    pub name: String,
    /// `0=provisioning, 1=active, 2=suspended, 3=deleted` — matches the
    /// `CHECK (status IN (0,1,2,3))` constraint in the migration DDL.
    pub status: i16,
    pub self_managed: bool,
    pub tenant_type_uuid: Uuid,
    pub depth: i32,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Soft-delete tombstone. Populated at the moment `schedule_deletion`
    /// flips the row to `status = Deleted`; cleared on un-delete. Also
    /// doubles as the retention-timer start: the hard-delete sweep
    /// becomes eligible at `deleted_at + retention_window_secs` (with the
    /// scanner-default substituted when the per-row override is NULL).
    #[sea_orm(nullable)]
    pub deleted_at: Option<OffsetDateTime>,
    /// Phase 3 — optional per-tenant override of the module-default
    /// retention window. Stored as BIGINT seconds (not INTERVAL) so the
    /// shape is portable across `SQLite` / `MySQL` / Postgres.
    #[sea_orm(nullable)]
    pub retention_window_secs: Option<i64>,
    /// Phase 5 — hard-delete worker claim. A non-NULL value means a
    /// retention scanner atomically claimed the row before processing it.
    #[sea_orm(nullable)]
    pub claimed_by: Option<Uuid>,
    /// Phase 5 — timestamp at which the current claim was made. The
    /// stale-claim TTL evaluates against this column rather than
    /// `updated_at` so worker-liveness detection is independent of any
    /// future patch path that bumps `updated_at` on a `Deleted`-status
    /// row. Cleared together with `claimed_by` when the scanner finishes
    /// the row.
    #[sea_orm(nullable)]
    pub claimed_at: Option<OffsetDateTime>,
    /// Phase 5 — operator-action-required marker for provisioning rows
    /// the `IdP` plugin classified as
    /// [`account_management_sdk::IdpDeprovisionFailure::Terminal`]. Once
    /// stamped, [`scan_stuck_provisioning`](super::super::repo_impl::retention::scan_stuck_provisioning)
    /// filters the row out of the reaper retry loop until an operator
    /// clears the column or hard-deletes the row. Always `None` for
    /// rows in any status other than `Provisioning` (terminal-failure
    /// is a state of the provisioning lifecycle, not a generic flag).
    #[sea_orm(nullable)]
    pub terminal_failure_at: Option<OffsetDateTime>,
}
// @cpt-end:cpt-cf-account-management-dbtable-tenants:p1:inst-dbtable-tenants-entity

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn root_model_has_no_parent_and_depth_zero() {
        let now = OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("stable epoch");
        let m = Model {
            id: Uuid::from_u128(0x10),
            parent_id: None,
            name: "root".into(),
            status: 1,
            self_managed: false,
            tenant_type_uuid: Uuid::from_u128(0x3),
            depth: 0,
            created_at: now,
            updated_at: now,
            deleted_at: None,
            retention_window_secs: None,
            claimed_by: None,
            claimed_at: None,
            terminal_failure_at: None,
        };
        assert!(m.parent_id.is_none());
        assert_eq!(m.depth, 0);
    }
}
