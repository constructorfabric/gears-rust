//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__order_line")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i32,
    #[sea_orm(primary_key, auto_increment = false)]
    pub line_id: Uuid,
    pub commercial_attempt_id: Option<Uuid>,
    pub pricing_acceptance_id: Option<Uuid>,
    pub pricing_request_digest: Option<String>,
    pub pricing_terms_digest: Option<String>,
    pub plan_id: Uuid,
    pub plan_revision_id: Uuid,
    pub selected_items: Json,
    pub currency: String,
    pub contract_effective_date: TimeDate,
    pub service_activation_date: Option<TimeDate>,
    pub acceptance_due_date: Option<TimeDate>,
    #[sea_orm(
        column_type = "custom(\"interval\")",
        select_as = "text",
        save_as = "interval"
    )]
    pub term_duration: Option<String>,
    pub term_kind: String,
    pub authored_term: Json,
    pub billing_cycle: String,
    pub order_pin: Option<Json>,
    pub overlap_scope_key: Option<String>,
    pub date_policy_switch_state: Json,
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
