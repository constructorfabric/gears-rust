//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Approval requirement or gate verdict reflected onto one order version.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__approval_reflection")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Entry identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub reflection_id: Uuid,
    /// Owning order.
    pub order_id: Uuid,
    /// Order version the verdict was decided against.
    pub version: i32,
    /// `requirement` or `gate_outcome`; at most one reflected fact of each kind per version.
    pub verdict_kind: String,
    /// `required`, `not_required`, `granted` or `denied`, as admitted by `verdict_kind`.
    pub verdict: String,
    /// Named deciding authority: the policy owner or an explicit stand-in.
    pub deciding_authority: String,
    /// Opaque reason received with a denied gate outcome; never evaluated.
    pub denial_reason: Option<String>,
    /// Sibling process correlation copied from the reflection request.
    pub correlation_id: Uuid,
    /// Reflection instant.
    pub reflected_at: TimeDateTimeWithTimeZone,
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
