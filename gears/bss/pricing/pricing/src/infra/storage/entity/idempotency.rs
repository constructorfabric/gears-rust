//! Scoped persistence model, following Products.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_idempotency")]
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
    /// matched (D-429).
    #[sea_orm(primary_key, auto_increment = false)]
    pub endpoint: String,
    /// Composite primary key with `tenant_id` and `endpoint`. The caller's
    /// own `Idempotency-Key`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub client_key: String,
    /// `claimed | answered`, constrained by
    /// `chk_pricing_idempotency_state`. `claimed` means "in flight" and
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
    /// never needs to dereference another row (D-429). `NULL` while
    /// `claimed`, `NOT NULL` once `answered`, together with
    /// `response_status`.
    pub response_body: Option<JsonValue>,
    /// The retention deadline, stamped at the claim `INSERT`, and also the
    /// compare-and-swap operand the expired-key takeover reads before it
    /// writes (D-429).
    pub expires_at: TimeDateTimeWithTimeZone,
    /// The durable op an unanswered claim is bound to (`bind_op`: POST entry, POST plan item); a bound
    /// `claimed` row is never taken over on expiry (D-429). `NULL` for every other door.
    pub entity_ref: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
