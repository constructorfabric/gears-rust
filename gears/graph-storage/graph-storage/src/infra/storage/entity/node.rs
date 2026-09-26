//! Graph nodes (`cpt-cf-graph-storage-dbtable-node`).
//!
//! Owned, reference and phantom nodes share this table; the distinction is
//! carried by the GTS type family, not by storage (ADR-0002).

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "node")]
#[secure(tenant_col = "tenant_id", resource_col = "id", no_owner, no_type)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub tenant_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    /// Producer-supplied stable key, unique within the tenant.
    pub node_key: String,
    /// Interned type reference into `gts_type`.
    pub gts_node_type_id: i32,
    pub name: String,
    /// GTS-validated attributes, bounded by the payload ceiling.
    pub payload: Json,
    /// Composed vectorizable text (lexical index source via the generated
    /// `search` tsvector column, which `SeaORM` does not map).
    pub search_text: String,
    /// Node embedding; `None` until a producer supplies one.
    pub embedding: Option<PgVector>,
    /// Embedding-space epoch the vector belongs to.
    pub embedding_epoch: Option<i64>,
    /// Canonical hash of the embedding input (staleness detection).
    pub embedding_input_hash: Option<String>,
    /// Source namespace for reference nodes; `NULL` for owned nodes.
    /// Written once on insert, never by an upsert.
    pub source_namespace: Option<String>,
    /// Producer principal that created the row; immutable after insert.
    pub owner_principal: String,
    /// Monotonic per-row version, the `expected_version` CAS target.
    pub version: i64,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    /// Soft-delete tombstone; `NULL` for live rows.
    pub deleted_at: Option<OffsetDateTime>,
    /// Subject that first wrote the row (DESIGN § API element envelope).
    pub created_by_subject_id: Uuid,
    pub created_by_subject_type: Option<String>,
    /// Subject of the most recent write.
    pub updated_by_subject_id: Uuid,
    pub updated_by_subject_type: Option<String>,
    /// Subject that tombstoned the row; `NULL` while live.
    pub deleted_by_subject_id: Option<Uuid>,
    pub deleted_by_subject_type: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
