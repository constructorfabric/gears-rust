//! `SeaORM` entity for `bss.ledger_invoice_exposure` (the per-invoice credit-note
//! **headroom** counter: `original_total_minor` seeded = posted AR incl. tax,
//! plus the running `debit_note_total_minor` / `credit_note_total_minor`, keyed
//! by `(tenant_id, invoice_id)`). Tenant-scoped via `SecureORM`; the resource col
//! is the business `invoice_id`.
//!
//! `credit_note_total_minor <= original_total_minor + debit_note_total_minor` is
//! the authoritative headroom guard (design §4.7 / §7, AC #24); the
//! `CreditNoteHandler` (Phase 1) bumps `credit_note_total_minor` by an in-place
//! delta under the lock order with the CHECK evaluated post-delta. The
//! `DebitNoteHandler` raises `debit_note_total_minor` to lift the cap.
//! `original_total_minor` is seeded at first touch via `INSERT … ON CONFLICT DO
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
    pub original_total_minor: i64,
    pub debit_note_total_minor: i64,
    pub credit_note_total_minor: i64,
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
