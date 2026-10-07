//! `SeaORM` entity for `bss.products_idempotency` — the at-most-once gate for
//! every mutating flow that carries an `Idempotency-Key`
//! (`design/01-foundation.md` §3.2, §4.4), keyed `(tenant_id, endpoint,
//! client_key)`.
//!
//! This entity lays down the storage shape only. The claim `INSERT` itself is
//! the concurrency gate (P-D-198) and lives in
//! [`crate::infra::storage::repo`], not here — see that module's
//! `claim_idempotency_key` for the mechanism this table exists to support.
//!
//! `response_status` and `response_body` are nullable together: `claimed`
//! means both `NULL`, `answered` means both `NOT NULL`, enforced by
//! `chk_products_idempotency_response_group` on both backends. An answer is
//! stored only when its transaction commits (P-D-198), so nothing here ever
//! reads a `claimed` row with a partial response as anything but a violation of
//! that `CHECK`.
//!
//! `expires_at` is stamped at the claim `INSERT` and does double duty: it is
//! the retention deadline and it is the claim stamp (P-D-198) — the operand
//! the expired-key takeover's compare-and-swap reads before it writes, so
//! that two duplicates racing to take over the same expired row cannot both
//! succeed. See the migration's own module doc for the full argument.
//!

use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "products_idempotency")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "client_key",
    no_owner,
    no_type
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    /// Composite primary key with `tenant_id` and `client_key`. The concrete
    /// resource path a wire caller resolved, never the route template it
    /// matched (P-D-198).
    #[sea_orm(primary_key, auto_increment = false)]
    pub endpoint: String,
    /// Composite primary key with `tenant_id` and `endpoint`. The caller's
    /// own `Idempotency-Key`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub client_key: String,
    /// `claimed | answered`, constrained by
    /// `chk_products_idempotency_state`. `claimed` means "in flight" and
    /// nothing more: an unanswered claim was rolled back with the mutation
    /// it shared a transaction with, so no committed row is ever left
    /// needing release.
    pub state: String,
    /// The canonical rendering's digest the claim was made against — never
    /// computed by this repository layer, only stamped and compared.
    pub payload_hash: Vec<u8>,
    /// The status the original caller was told. `NULL` while `claimed`,
    /// `NOT NULL` once `answered`, together with `response_body`.
    pub response_status: Option<i32>,
    /// The body the original caller was told, self-contained so a replay
    /// never needs to dereference another row (P-D-198). `NULL` while
    /// `claimed`, `NOT NULL` once `answered`, together with
    /// `response_status`.
    pub response_body: Option<JsonValue>,
    /// The retention deadline, stamped at the claim `INSERT`, and also the
    /// compare-and-swap operand the expired-key takeover reads before it
    /// writes (P-D-198).
    pub expires_at: TimeDateTimeWithTimeZone,
    /// Carried in the DDL (P-D-193) and always `NULL`: no door stamps it.
    pub entity_ref: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
