//! `SeaORM` entity for `bss.ledger_recognition_schedule` (the ASC 606 documented
//! release plan for one single-revenue-stream deferred Contract-liability
//! balance: links to the originating posted invoice item, PO/allocation group,
//! currency, total deferred + recognized-to-date, immutable policy/SSP/VC refs,
//! and status, keyed by `(tenant_id, schedule_id)`). Tenant-scoped via
//! `SecureORM`; the resource col is the business `schedule_id` (mirrors
//! `dispute`'s `dispute_id`).
//!
//! `recognized_minor <= total_deferred_minor` is the authoritative
//! over-recognition guard (design §7); the `RecognitionRunner` (Phase 2) bumps
//! `recognized_minor` by an in-place delta under the lock order with the CHECK
//! evaluated post-delta. The partial `UNIQUE (tenant_id, source_invoice_id,
//! source_invoice_item_ref, revenue_stream) WHERE status='ACTIVE'` is the
//! at-most-one-live guard; build-idempotency is decoupled from `status` and
//! lives in `idempotency_dedup` (Rev3 / S4-F2), so a terminal `COMPLETED`
//! schedule stays archivable without re-opening a duplicate-build hole.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "ledger_recognition_schedule")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "schedule_id",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub schedule_id: String,
    pub payer_tenant_id: Uuid,
    pub source_invoice_id: String,
    pub source_invoice_item_ref: String,
    pub po_allocation_group: Option<String>,
    pub subscription_ref: Option<String>,
    pub revenue_stream: String,
    pub currency: String,
    pub total_deferred_minor: i64,
    pub recognized_minor: i64,
    pub policy_ref: String,
    pub ssp_snapshot_ref: Option<String>,
    pub vc_estimate_ref: Option<String>,
    pub vc_method_ref: Option<String>,
    pub status: String,
    pub version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
