//! `SeaORM` entity for `bss.products_audit_log` — the append-only audit trail
//! (P-D-193, P-D-200).
//!
//! # The reserved platform-sealing seam
//!
//! `seal_state`, `chain_id`, `seq`, `prev_hash` and `row_hash` exist so the
//! platform sealing capability (P-D-200) can activate without a migration.
//! `seal_state` is written `unsealed` at INSERT, always; this gear computes
//! no hash and runs no verification job — that is the platform capability's
//! job. The one admitted `UPDATE`, the one-way `unsealed -> sealed`
//! transition, is enforced by the table's trigger, not by this entity: this
//! entity carries no write rule for it at all.
//!

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_audit_log")]
#[secure(tenant_col = "tenant_id", resource_col = "audit_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub audit_id: Uuid,
    pub tenant_id: Uuid,
    /// The acting principal from `SecurityContext::subject_id()`; no actor table.
    pub actor_ref: Uuid,
    /// The audit action token (`design/01-foundation.md` §4.4). No
    /// vocabulary `CHECK` yet — an owed debt the migration's own doc names.
    pub action: String,
    /// The kind of thing `subject_id` names. Same owed debt as `action`.
    pub subject_kind: String,
    /// The subject's id. Nullable in the DDL; the one writer,
    /// `write_eventless_act_audit`, sets it on every row.
    pub subject_id: Option<Uuid>,
    /// The subject's revision at the time of the act, where the caller passes
    /// one: the rows `governance::audit` writes (approval policy, approval
    /// unit, reference and unfence acts) carry `NULL`.
    pub subject_revision: Option<i64>,
    /// Carried in the DDL and always `NULL`: no door writes a refusal row
    /// (P-D-200).
    pub error_code: Option<String>,
    /// Carried in the DDL and always `NULL` (P-D-200).
    pub attempted_key: Option<String>,
    /// A free-text reason, where the door supplies one.
    pub reason: Option<String>,
    /// The request's correlation, as `text` (P-D-200). Products writes `None`
    /// on every row: this gear establishes no request correlation.
    pub correlation_id: Option<String>,
    /// The act's instant as its writer took it (before the transaction or inside the attempt; never the
    /// commit) — P-D-213. The history orders by `audit_id`, not by this.
    pub written_at: TimeDateTimeWithTimeZone,
    /// Carried in the DDL; no writer sets it, so it is always `NULL`.
    pub session_id: Option<Uuid>,
    /// Carried in the DDL (P-D-193); no writer sets it, so it is always `NULL`.
    pub ceremony_ref: Option<Uuid>,
    /// `unsealed | sealed`. Written `unsealed` at INSERT, always; this gear
    /// never advances it.
    pub seal_state: String,
    /// Reserved for the platform sealing capability. `NULL` until sealed.
    pub chain_id: Option<Uuid>,
    /// Reserved for the platform sealing capability. `NULL` until sealed.
    pub seq: Option<i64>,
    /// Reserved for the platform sealing capability. `NULL` on the segment
    /// head and until sealed.
    pub prev_hash: Option<Vec<u8>>,
    /// Reserved for the platform sealing capability. `NULL` until sealed.
    pub row_hash: Option<Vec<u8>>,
    /// The SKU lifecycle the act found (P-D-213, `m20260927_000008`): one of the five, held by a
    /// named `CHECK`. `NULL` on a row whose act concerns no SKU, on a create (the SKU did not exist)
    /// and on every row written before the migration.
    pub from_lifecycle: Option<String>,
    /// The SKU lifecycle the act left (P-D-213). `NULL` on a row whose act concerns no SKU, on a
    /// draft delete (the SKU no longer exists) and on every row written before the migration.
    pub to_lifecycle: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
