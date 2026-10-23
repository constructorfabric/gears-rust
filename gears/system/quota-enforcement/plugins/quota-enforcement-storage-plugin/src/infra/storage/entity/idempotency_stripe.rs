//! `qe_idempotency_stripes`: the fixed set of rows that serialize writers of
//! one idempotency scope (invariant I8). A scope maps to its stripe by
//! [`stripe_of`](crate::infra::storage::repo::idempotency_repo::stripe_of);
//! the rows are created by migration and never inserted or deleted at run
//! time, so locking one is always a plain `FOR UPDATE NOWAIT`.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

/// How many stripes exist. Unrelated scopes that share a stripe serialize on
/// it, so this trades table size for fewer false conflicts. Changing it remaps
/// every scope: only with a migration that recreates the rows, and never while
/// two versions run side by side.
pub const STRIPES: u32 = 65_536;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_idempotency_stripes")]
#[secure(no_tenant, no_resource, no_owner, no_type)]
pub struct Model {
    /// `0..STRIPES`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub stripe: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
