//! `qe_charge_entries`: one row per Debit Plan entry of a charge.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "qe_charge_entries")]
#[secure(tenant_col = "tenant_id", resource_col = "quota_id", no_owner, no_type)]
pub struct Model {
    /// The charge.
    #[sea_orm(primary_key, auto_increment = false)]
    pub charge_id: Uuid,
    /// Chain of the plan entry (ADR-0010).
    #[sea_orm(primary_key, auto_increment = false)]
    pub chain: i32,
    /// Position of the entry inside its chain, primary pool first.
    #[sea_orm(primary_key, auto_increment = false)]
    pub position: i32,
    /// The charge's tenant, so `SecureORM` scopes the row.
    pub tenant_id: Uuid,
    /// The charged Quota.
    pub quota_id: Uuid,
    /// The period row a consumption entry was charged to.
    pub period_id: Option<Uuid>,
    /// What the entry charged.
    pub charged: i64,
    /// What has been returned of it; never above `charged`.
    pub returned: i64,
    /// The entry keeps its charge alive: an allocation entry with
    /// `returned < charged` whose Quota's holds have not been released.
    pub holding: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
