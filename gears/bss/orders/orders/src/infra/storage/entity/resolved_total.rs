//! Storage row; never a public SDK model.
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
/// Producer-resolved money total of an order version, per line or rolled up per order.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_orders__resolved_total")]
#[secure(no_tenant, resource_col = "order_id", no_owner, no_type)]
pub struct Model {
    /// Owning order.
    #[sea_orm(primary_key, auto_increment = false)]
    pub order_id: Uuid,
    /// Owning order version.
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i32,
    /// `line` or `order`; distinguishes the roll-up from a per-line row.
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope: String,
    /// The line, or the zero UUID for the order-level roll-up.
    #[sea_orm(primary_key, auto_increment = false)]
    pub line_id: Uuid,
    /// ISO 4217 currency code.
    pub currency: String,
    /// Pricing assessment shared by every contributing order pin.
    pub assessment_id: Uuid,
    /// Producer-supplied currency scale, 0 to 28; display estimates carry at most this many
    /// fractional digits (D-216).
    pub currency_scale: i32,
    /// Explicit usage, tax and unsupported-overlay exclusions; discount availability is
    /// `discount_status`.
    pub exclusions: Json,
    /// Producer-supplied per-item amounts.
    pub item_breakdown: Json,
    /// Producer-supplied recurring amounts labeled by cycle.
    pub recurring_by_cycle: Json,
    /// `committed` for recurring or one-time charges, `uncommitted_usage` for usage.
    pub amount_status: String,
    /// Producer-declared monetary horizon.
    pub amount_basis: String,
    /// Producer line, term and cycle evidence for the amount.
    pub period_evidence: Json,
    /// Exact producer gross amount, canonical decimal text in major units; NULL for usage.
    pub gross_exact: Option<String>,
    /// Producer major-unit display estimate of the gross amount at currency scale; NULL for usage.
    pub gross_display: Option<String>,
    /// Exact producer net amount, canonical decimal text in major units; NULL for usage.
    pub net_exact: Option<String>,
    /// Producer major-unit display estimate of the net amount at currency scale; NULL for usage.
    pub net_display: Option<String>,
    /// `available` or `unavailable`; an unavailable discount is never a zero (D-197, D-216).
    pub discount_status: String,
    /// Exact producer discount, present exactly when `discount_status` is `available`.
    pub discount_exact: Option<String>,
    /// Display estimate of the discount, present exactly when the discount is available.
    pub discount_display: Option<String>,
    /// Applied promotion, if any.
    pub promotion_ref: Option<String>,
    /// `recurring`, `usage` or `one_time`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub charge_kind: String,
    /// Exact producer total contract value, net pre-tax; only on the order-level recurring row.
    pub tcv_exact: Option<String>,
    /// Display estimate of the total contract value; only on the order-level recurring row.
    pub tcv_display: Option<String>,
    /// `finite_term`, `rolling_annualized` or `mixed`; only on the order-level recurring row.
    pub tcv_basis: Option<String>,
    /// Per-line TCV basis and producer policy version; only on the order-level recurring row.
    pub tcv_evidence: Option<Json>,
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
