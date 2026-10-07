//! `SeaORM` entity for the `credstore_store_cleanup` table (ADR-0006): the
//! cleanup debts, one row per obligation on the value store.
//!
//! Internal bookkeeping, not a resource: it is never exposed and carries no
//! authorization dimension, so the entity is `unrestricted` and accessed with
//! the unconstrained scope. Read by point lookups on `(tenant_id, record_id)`
//! only.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db::secure::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "credstore_store_cleanup")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The store key `(tenant_id, record_id)` the debt applies to.
    pub tenant_id: Uuid,
    pub record_id: Uuid,
    /// `1` = purge (`delete_key`), `2` = destroy.
    pub op: i16,
    /// Destroy only: `1` = below, `2` = exact; `NULL` for purge.
    pub selector: Option<i16>,
    /// Destroy only: the value version; `NULL` for purge.
    pub version: Option<String>,
    /// Database clock.
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
