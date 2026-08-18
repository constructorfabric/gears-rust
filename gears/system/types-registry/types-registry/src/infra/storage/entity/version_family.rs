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

// ponytail: ceiling C6 — no PDP. P0 reads and writes are authenticated but not
// authorized, which deviates from `06_authn_authz_secure_orm.md`'s "every sensitive
// DB access MUST be covered by a PDP decision". `unrestricted` is chosen here
// rather than `tenant_col = "owner_tenant_id"` because P0 never populates tenant
// scope: every row carries `ownership_scope = 1` and a NULL owner, so a
// tenant-scoped predicate would match nothing and `unrestricted` states that
// intent instead of faking a dimension. It fails closed either way — a
// tenant-scoped query against a tenant-scoped entity with an empty scope yields
// `WHERE 1=0`.
//
// Upgrade path, and the reason the column exists already: switch this attribute
// to `#[secure(tenant_col = "owner_tenant_id", ...)]` and add the
// `PolicyEnforcer` calls once the identity-to-permission binding lands (SPEC §9
// C6, §12). The DDL needs no migration for that step.
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
