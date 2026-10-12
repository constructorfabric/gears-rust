//! `SeaORM` entity for `bss.ledger_invoice_exposure` (the per-invoice credit-note
//! **headroom** counter: `original_total` seeded = posted AR incl. tax,
//! plus the running `debit_note_total` / `credit_note_total`, keyed
//! by `(tenant_id, invoice_id)`). Tenant-scoped via `SecureORM`; the resource col
//! is the business `invoice_id`.
//!
//! `credit_note_total <= original_total + debit_note_total` is
//! the authoritative headroom guard (design §4.7 / §7, AC #24); the
//! `CreditNoteHandler` (Phase 1) bumps `credit_note_total` by an in-place
//! delta under the lock order with the CHECK evaluated post-delta. The
//! `DebitNoteHandler` raises `debit_note_total` to lift the cap.
//! `original_total` is seeded at first touch via `INSERT … ON CONFLICT DO
//! UPDATE` (the Slice 1 first-touch upsert), so concurrent creators serialize.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_invoice_exposure")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "invoice_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub invoice_id: String,
    pub currency: String,
    pub currency_scale: i16,
    pub original_total: String,
    pub debit_note_total: String,
    pub credit_note_total: String,
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
