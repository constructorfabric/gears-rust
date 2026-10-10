//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__gate_outcome")]
#[secure(
    tenant_col = "subject_tenant_id",
    resource_col = "run_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub outcome_id: Uuid,
    pub run_id: Uuid,
    pub subject_tenant_id: Uuid,
    pub subject_id: String,
    pub resource_tenant_id: Uuid,
    pub seller_tenant_id: Uuid,
    pub payer_tenant_id: Uuid,
    pub correlation_id: Option<Uuid>,
    pub order_id: Option<Uuid>,
    pub version: Option<i32>,
    pub line_id: Option<Uuid>,
    pub predicate: String,
    pub item_id: Option<Uuid>,
    pub has_catalog_selection: bool,
    pub catalog_scope_key: Option<String>,
    pub verdict: String,
    pub reason: Option<String>,
    pub mapping_version: String,
    pub applicability: String,
    pub producer_results: Json,
    pub upstream_detail: Option<Json>,
    pub evaluated_at: TimeDateTimeWithTimeZone,
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
