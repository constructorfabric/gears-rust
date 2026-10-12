//! The embedding-space registry (`cpt-cf-graph-storage-dbtable-embedding-space`).
//!
//! One row per embedding space the deployment has ever opened; at most one in
//! state `active`, enforced by a partial unique index rather than by the boot
//! path remembering to check. `node.embedding_epoch` points here, so this is
//! what makes "only vectors of the active space are searchable" expressible.
//!
//! `tenant_id` holds the nil UUID: the table is deployment-wide, and the
//! column exists so the row is reachable through the secure ORM at all. See
//! the migration and DESIGN § 3.7 for why.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[expect(
    clippy::struct_field_names,
    reason = "`model_artifact` is DESIGN's column name and shares a prefix with \
              SeaORM's mandatory `Model` struct name; neither is ours to rename"
)]
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "embedding_space")]
#[secure(tenant_col = "tenant_id", no_resource, no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub epoch: i64,
    pub identity_hash: String,
    pub model_artifact: String,
    pub tokenizer_artifact: String,
    pub preprocessing: Json,
    pub pooling: Json,
    pub normalization: Json,
    pub dimension: i32,
    pub state: String,
    pub created_at: TimeDateTimeWithTimeZone,
    pub activated_at: Option<TimeDateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
