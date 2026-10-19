//! `bss_rating__operation` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-operation`).
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__operation")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// Key scope `(tenant_id, actor, idempotency_key)`.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub actor: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub idempotency_key: String,
    pub operation_id: Uuid,
    /// Lowercase hex SHA-256 of the canonical request.
    pub request_digest: String,
    /// The receipt a retry with the same key and body is answered with.
    pub response: JsonValue,
    /// A key is honoured for 7 days after this instant.
    pub created_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
