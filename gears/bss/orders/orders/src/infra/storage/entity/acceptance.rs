//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Recorded acceptance of one committed order version's terms.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__acceptance")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Owning order.
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    /// Committed order version whose terms were accepted; never inferred from a later version.
    #[sea_orm(primary_key, auto_increment = false)]
    pub accepted_version: i32,
    /// Recorded acceptance instant; never defaulted.
    pub accepted_at: TimeDateTimeWithTimeZone,
    /// Actor who recorded the acceptance.
    pub recorded_by: String,
    /// How the acceptance was recorded: `self_service` or `partner_placed`.
    pub recording_path: String,
    /// Why acceptance was required: `contract`, `seller`, `platform_default` or `volunteered`.
    pub requirement_source: String,
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
