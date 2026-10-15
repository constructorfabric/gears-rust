//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__read_access_log")]
#[secure(
    no_tenant,
    resource_col = "access_id",
    no_owner,
    no_type,
    pep_prop(actor = "actor")
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub access_id: Uuid,
    pub order_id: Option<Uuid>,
    pub requested_order_ref: Option<Uuid>,
    pub actor: String,
    pub actor_class: String,
    pub operation: String,
    pub outcome: String,
    pub refusal_reason: Option<String>,
    pub internal_refusal_detail: Option<String>,
    pub delegation_proof_ref: Option<String>,
    pub accessed_at: TimeDateTimeWithTimeZone,
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
