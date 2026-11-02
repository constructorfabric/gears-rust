//! `bss_rating__pricing_resolve` (DESIGN §3.7 `cpt-cf-bss-rating-dbtable-pricing-resolve`): one row
//! per query per UTC day it was fetched; insert-only.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__pricing_resolve")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    /// The work item's seller tenant.
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub query_digest: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub fetched_on: TimeDate,
    pub catalog_tenant_id: Uuid,
    pub revision_id: Uuid,
    pub resolve_date: TimeDate,
    pub item_id: Option<Uuid>,
    pub pins: JsonValue,
    pub binding_digest: String,
    pub fetched_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
