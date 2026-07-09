//! `SeaORM` entity for the `audit_outbox` table.
//!
//! Rows are inserted in the same DB transaction as the write they describe
//! and drained asynchronously to the platform audit sink.
//!
//! @cpt-cf-file-storage-fr-audit-trail
//! @cpt-cf-file-storage-nfr-audit-completeness

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// A single audit outbox row.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "audit_outbox")]
#[secure(no_tenant, resource_col = "event_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_id: Uuid,
    pub tenant_id: Uuid,
    pub actor_kind: String,
    pub actor_id: Uuid,
    pub file_id: Option<Uuid>,
    pub operation: String,
    pub outcome: String,
    #[sea_orm(column_type = "Json")]
    pub detail: Json,
    pub occurred_at: OffsetDateTime,
    pub published_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
