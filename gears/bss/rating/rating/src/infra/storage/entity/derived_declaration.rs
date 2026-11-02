//! `bss_rating__derived_declaration` (DESIGN §3.7
//! `cpt-cf-bss-rating-dbtable-derived-declaration`): Products' declaration Rating rated with,
//! verbatim with its digest; insert-only.
use sea_orm::entity::prelude::*;
use serde_json::Value as JsonValue;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "bss_rating__derived_declaration")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub code: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub version: i64,
    /// SHA-256 of Products' canonical declaration bytes.
    pub digest: String,
    pub declaration: JsonValue,
    pub fetched_at: TimeDateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
