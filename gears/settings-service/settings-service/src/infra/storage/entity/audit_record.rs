// Created: 2026-09-07 by Constructor Tech
//! `audit_records`: the gear-local audit store. Append-only by contract — no
//! code path updates a row, and the only delete is retention pruning.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

/// One audit record.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "audit_records")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub resource: String,
    pub declaration_key: String,
    pub tenant_id: Uuid,
    pub operation: String,
    pub actor: String,
    pub actor_classification: String,
    #[sea_orm(nullable)]
    pub pre_value: Option<Json>,
    #[sea_orm(nullable)]
    pub post_value: Option<Json>,
    pub outcome: String,
    pub request_id: String,
    #[sea_orm(nullable)]
    pub change_set_id: Option<Uuid>,
    pub occurred_at: OffsetDateTime,
    #[sea_orm(nullable)]
    pub retain_until: Option<OffsetDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
