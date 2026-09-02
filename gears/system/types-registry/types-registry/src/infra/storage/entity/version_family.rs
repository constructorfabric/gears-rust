//! `types_registry__version_family` — binds a family key to one ownership scope.
//!
//! Mirror of the table in `docs/database.sql`. The row has no newest member,
//! current pointer, count, highest major or family-wide version: it exists to
//! enforce common ownership and to serialize concurrent first admission
//! (ADR-0004, ADR-0008).
//!
//! `family_key` is not a GTS Identifier — it is the canonical identifier with the
//! whole version of its **last** segment removed and the trailing `~` normalized
//! away — so it MUST NOT be parsed as one.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

use super::enums::OwnershipScope;

// ponytail: ceiling C6 — no PDP. `unrestricted` for the reason given in full on
// `entity`: P0 never populates tenant scope, so a `tenant_col = "owner_tenant_id"`
// predicate would match nothing. Same upgrade path, and no DDL migration for it.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__version_family")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub family_key: String,
    pub ownership_scope: OwnershipScope,
    pub owner_tenant_id: Option<Uuid>,
    pub created_at: OffsetDateTime,
}

/// No relations are declared. `entity.family_id` is a real foreign key, but
/// nothing joins across it yet; the repositories of T4 read the family by key
/// under its own lock. Declaring an unused `has_many` would be code with no
/// reader.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
