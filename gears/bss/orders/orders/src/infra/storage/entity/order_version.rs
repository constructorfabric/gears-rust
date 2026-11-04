//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Immutable committed version of an order.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__order_version")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Owning order.
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    /// Committed version number; gaps are allowed after abandoned reservations.
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i32,
    /// Previous committed version; NULL on version 1.
    pub supersedes_version: Option<i32>,
    /// Derived order market currency at this version.
    pub market_currency: Option<String>,
    /// Derived order market region at this version.
    pub market_region: Option<String>,
    /// Payer tenant at this version.
    pub payer_tenant_id: Uuid,
    /// Order category GTS identifier captured at this version.
    pub category: String,
    /// Contract reference at this version.
    pub contract_id: Option<Uuid>,
    /// Actor that appended this version.
    pub actor: String,
    /// Appending actor's trusted subject tenant.
    pub actor_tenant_id: Uuid,
    /// `create`, `submit` or `amendment`.
    pub reason: String,
    /// Caller explanation on an amendment; NULL on create and submit.
    pub amendment_reason: Option<String>,
    /// Append instant.
    pub created_at: TimeDateTimeWithTimeZone,
}
/// `SeaORM` relations of this table.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    /// Belongs to the owning order in `bss_orders__order`.
    #[sea_orm(
        belongs_to = "super::order::Entity",
        from = "Column::OrderId",
        to = "super::order::Column::OrderId"
    )]
    Order,
}
impl Related<super::order::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Order.def()
    }
}
impl ActiveModelBehavior for ActiveModel {}
