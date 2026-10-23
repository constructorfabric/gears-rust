//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Mutable administrative content of an order line, kept off the version chain.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__order_line_admin")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Owning order.
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    /// Order-scoped line identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub line_id: Uuid,
    /// Free-text external reference, at most 1024 characters.
    pub external_reference: Option<String>,
    /// Free-text display label, at most 256 characters.
    pub display_label: Option<String>,
    /// Free-text internal notes, at most 4096 characters.
    pub internal_notes: Option<String>,
    /// Actor of the last edit.
    pub updated_by: String,
    /// Instant of the last edit.
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
