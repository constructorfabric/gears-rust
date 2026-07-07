//! `SeaORM` entity for `bss.ledger_journal_line` (append-only truth detail).

use chrono::NaiveDate;
use sea_orm::entity::prelude::*;
use uuid::Uuid;

use toolkit_db_macros::Scopable;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_journal_line")]
#[secure(tenant_col = "tenant_id", resource_col = "line_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub line_id: Uuid,
    pub entry_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub period_id: String,
    pub payer_tenant_id: Uuid,
    pub seller_tenant_id: Option<Uuid>,
    pub resource_tenant_id: Option<Uuid>,
    pub account_id: Uuid,
    pub account_class: String,
    pub gl_code: Option<String>,
    pub side: String,
    pub amount_minor: i64,
    pub currency: String,
    pub currency_scale: i16,
    pub invoice_id: Option<String>,
    pub due_date: Option<NaiveDate>,
    pub revenue_stream: Option<String>,
    pub mapping_status: String,
    pub functional_amount_minor: Option<i64>,
    pub functional_currency: Option<String>,
    pub tax_jurisdiction: Option<String>,
    pub tax_filing_period: Option<String>,
    pub tax_rate_ref: Option<String>,
    pub legal_entity_id: Option<Uuid>,
    pub invoice_item_ref: Option<String>,
    pub sku_or_plan_ref: Option<String>,
    pub price_id: Option<String>,
    pub pricing_snapshot_ref: Option<String>,
    pub po_allocation_group: Option<String>,
    pub credit_grant_event_type: Option<String>,
    /// AR dispute sub-class snapshot (`ACTIVE`/`DISPUTED`), set on AR lines that
    /// participate in a chargeback reclass; `NULL` on every other line.
    pub ar_status: Option<String>,
    /// Locked FX rate for this line (FK → `ledger_fx_rate_snapshot.rate_id`);
    /// set on cross-currency posts, `NULL` on single-currency lines (Slice 5).
    pub rate_snapshot_ref: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
