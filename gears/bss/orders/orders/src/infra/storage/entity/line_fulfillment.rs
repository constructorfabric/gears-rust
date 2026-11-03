//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Fulfillment projection of one order line.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__line_fulfillment")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Owning order.
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    /// Order-scoped line identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub line_id: Uuid,
    /// Order version whose line was fulfilled.
    pub version: i32,
    /// `created`, `activated` or `failed`; a projection, not a state machine.
    pub status: String,
    /// Spawned subscription, one per line.
    pub subscription_id: Option<Uuid>,
    /// Downstream correlation reference; carries no state meaning.
    pub transition_request_ref: Option<String>,
    /// Last acknowledgement instant.
    pub updated_at: TimeDateTimeWithTimeZone,
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
