//! `qe_charge_returns`: the return record of one applied return (ADR-0011).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_charge_returns")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// The charge's tenant.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// The return's own key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub return_key: String,
    /// The charge the return gave back from.
    pub charge_id: Uuid,
    /// The returned amount.
    pub amount: i64,
    /// When the return was applied.
    pub created_at: OffsetDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
