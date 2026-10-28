//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Mutable authored content of one draft line; removed once submit materializes it.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__draft_content")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Owning order.
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    /// Order-scoped line identity.
    #[sea_orm(primary_key, auto_increment = false)]
    pub line_id: Uuid,
    /// Opaque authored plan reference.
    pub plan_id: Uuid,
    /// Opaque authored plan revision reference.
    pub plan_revision_id: Uuid,
    /// Selected items, optional choices, quantities and dimension values.
    pub selected_items: Json,
    /// The line's ISO 4217 currency code.
    pub currency: String,
    /// Authored contract-effective date, if authored yet.
    pub contract_effective_date: Option<TimeDate>,
    /// Authored requested service-activation date, if any.
    pub service_activation_date: Option<TimeDate>,
    /// Authored acceptance-due calendar date, if any.
    pub acceptance_due_date: Option<TimeDate>,
    /// Finite term as PostgreSQL interval text; NULL if rolling, missing or the cycle is unset.
    #[sea_orm(
        column_type = "custom(\"interval\")",
        select_as = "text",
        save_as = "interval"
    )]
    pub term_duration: Option<String>,
    /// Term classification: `missing`, `rolling` or `finite`.
    pub term_kind: String,
    /// Authored term intent as JSON, kept even when no interval can be derived yet.
    pub authored_term: Json,
    /// `month` or `year`; NULL while not yet authored in draft.
    pub billing_cycle: Option<String>,
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
