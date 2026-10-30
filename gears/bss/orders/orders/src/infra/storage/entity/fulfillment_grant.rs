//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Immutable fulfillment dispatch authority issued by spawn, resume or internal rebuild (D-198).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__fulfillment_grant")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Grant identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub grant_id: Uuid,
    /// Owning order.
    pub order_id: Uuid,
    /// Committed order version the grant dispatches.
    pub order_version: i32,
    /// Fulfillment attempt this grant authorizes.
    pub fulfillment_attempt_id: Uuid,
    /// Positive fulfillment control generation; unique per order.
    pub generation: i64,
    /// Digest of the roster and its receipts.
    pub roster_receipt_digest: String,
    /// Exact receiver, line and create-key roster, as a JSON array.
    pub roster: Json,
    /// Digest of the authenticated authority and facts behind the grant.
    pub authority_fact_digest: String,
    /// Grant this one supersedes; NULL for the first spawn.
    pub predecessor_grant_id: Option<Uuid>,
    /// Creation instant.
    pub created_at: TimeDateTimeWithTimeZone,
    /// Idempotency execution that issued the grant.
    pub execution_id: Uuid,
    /// Committed transition audit entry that issued the grant.
    pub audit_id: Uuid,
    /// Always `committed`; completes the composite FK to the audit entry.
    pub audit_outcome: String,
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
