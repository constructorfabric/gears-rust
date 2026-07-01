//! `SeaORM` entity for the `events_outbox` table.
//!
//! Rows are enqueued in the same DB transaction as the write they describe.
//! Relay to `EventBroker` is DEFERRED — the `EventBroker` gear is not yet present
//! in this repo (P2-M5 TODO).
//!
//! @cpt-cf-file-storage-fr-file-events

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// A single file-event outbox row.
#[allow(unknown_lints, de0309_must_have_domain_model)]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "events_outbox")]
#[secure(no_tenant, resource_col = "event_id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub event_id: Uuid,
    pub tenant_id: Uuid,
    pub owner_id: Uuid,
    pub file_id: Uuid,
    pub event_type: String,
    #[sea_orm(column_type = "Json")]
    pub payload: Json,
    pub occurred_at: OffsetDateTime,
    pub published_at: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
