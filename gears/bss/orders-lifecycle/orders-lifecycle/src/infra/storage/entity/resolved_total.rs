//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__resolved_total")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub line_id: Uuid,
    pub currency: String,
    pub assessment_id: Uuid,
    pub currency_minor_digits: i32,
    pub rounding_policy: String,
    pub exclusions: Json,
    pub item_breakdown: Json,
    pub recurring_by_cycle: Json,
    pub amount_status: String,
    pub amount_basis: String,
    pub period_evidence: Json,
    pub gross_minor: Option<i64>,
    pub net_minor: Option<i64>,
    pub discount_minor: Option<i64>,
    pub promotion_ref: Option<String>,
    #[sea_orm(primary_key, auto_increment = false)]
    pub charge_kind: String,
    pub tcv_minor: Option<i64>,
    pub tcv_basis: Option<String>,
    pub tcv_evidence: Option<Json>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
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
