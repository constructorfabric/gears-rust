// Created: 2026-10-03 by Constructor Tech
// Updated: 2026-10-06 by Constructor Tech
//! `SeaORM` entity for the `credstore_write_intents` table (ADR-0006): the
//! journal of in-flight secret write attempts.
//!
//! Internal bookkeeping, not a resource: it is never exposed and carries no
//! authorization dimension, so the entity is `unrestricted` and accessed with
//! the unconstrained scope. `lease_until` is always produced and compared by
//! the database clock (see the repository), never bound from the process.
//! Read by point lookups only: by `(tenant_id, record_id)` and by
//! `(tenant_id, reference)`.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db::secure::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "credstore_write_intents")]
#[secure(unrestricted)]
pub struct Model {
    /// Minted per write attempt (v4), never reused.
    #[sea_orm(primary_key, auto_increment = false)]
    pub attempt_id: Uuid,
    /// The store key `(tenant_id, record_id)` the attempt will `put` under.
    pub tenant_id: Uuid,
    pub record_id: Uuid,
    /// The record's reference: lets a failed create, which leaves no record
    /// row, be found and healed by reference.
    pub reference: String,
    /// Nobody may heal the intent before this instant (database clock).
    pub lease_until: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
