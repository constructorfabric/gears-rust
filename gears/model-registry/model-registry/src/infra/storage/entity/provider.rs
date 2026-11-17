//! `model_registry__providers` table (DESIGN §3.7).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "model_registry__providers")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "id",
    no_owner,
    no_type,
    pep_prop(gts_type = "gts_type")
)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub slug: String,
    pub name: String,
    pub gts_type: String,
    pub enabled: bool,
    /// `tenant` or `shared`.
    pub sharing_mode: String,
    pub managed: bool,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub metadata: Option<serde_json::Value>,
    /// Row version: 1 on create, incremented by every update.
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub created_by: Uuid,
    pub updated_at: DateTime<Utc>,
    pub updated_by: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::model::Entity")]
    Model,
}

impl Related<super::model::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Model.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
