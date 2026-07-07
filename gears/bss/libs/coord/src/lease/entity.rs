//! `coord_leases` — the `SeaORM` entity backing the distributed lease.
//!
//! One row per coordination domain (`key`); a worker holds the gate iff
//! `locked_by = <its uuid>` AND `locked_until > NOW()` on the DB clock. The
//! `key` PK lets unrelated singleton jobs share one table — e.g. a billing gear
//! keys recognition runs `"recognition-run:{tenant}:{period}"` while another
//! domain uses its own namespace.
//!
//! Schema (per dialect; created by the `coord::migration` migration the
//! consuming gear adds to its `Migrator`):
//!
//! * `key`          `TEXT` PRIMARY KEY               — coordination domain.
//! * `locked_by`    `UUID` / `TEXT` NULL             — current holder; `NULL` ≡ free.
//! * `locked_until` `TIMESTAMPTZ` / `TEXT` NOT NULL  — DB-clock expiry; epoch when free.
//! * `attempts`     `INTEGER` NOT NULL DEFAULT `0`   — forensic takeover counter.
//!
//! All time comparisons run on the DB clock via dialect SQL exprs (see
//! [`super::manager`]); the `DateTime<Utc>` field is only the worker's view of
//! `locked_until`, written on the acquire INSERT — the row's truth is whatever
//! the DB committed.
//!
//! `#[secure(no_tenant, no_resource, no_owner, no_type)]` because the row is
//! process-coordination state, not a tenant resource: every access scopes with
//! [`toolkit_security::AccessScope::allow_all`]. It is never surfaced through
//! any SDK — only this crate reads or writes it.

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "coord_leases")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    /// Coordination domain. A `TEXT` PK leaves room for many singleton jobs in
    /// one table with no schema work; the holder namespaces its own keys.
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    /// Current holder's worker id; `NULL` when the row is free.
    pub locked_by: Option<Uuid>,
    /// DB-clock expiry. When `locked_by IS NULL` this holds the epoch sentinel
    /// (`1970-01-01T00:00:00Z`) — kept non-nullable to simplify the
    /// `WHERE locked_until < NOW()` steal filter.
    pub locked_until: DateTime<Utc>,
    /// Monotonically increases on every steal (expired-lease takeover). Reset to
    /// `0` only on a clean `release`; `release_with_retry` preserves it as a
    /// forensic streak so a flapping holder is visible to operators.
    pub attempts: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
