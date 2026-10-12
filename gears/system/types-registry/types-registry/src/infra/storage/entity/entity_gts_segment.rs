//! `types_registry__entity_gts_segment` — one row per parsed segment of an
//! entity's `gts_id`, the columns discovery's SQL pattern filter reads.
//!
//! Mirror of the table in `docs/database.sql`. Written with the entity, in the
//! admission transaction, and never updated: `gts_id` is immutable.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;

// ponytail: ceiling C6 — no PDP, as on `entity`. Ownership is the parent's; the
// discovery join reaches rows only through an already scoped entity.
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__entity_gts_segment")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub entity_id: i64,
    /// 0-based position in the chain.
    #[sea_orm(primary_key, auto_increment = false)]
    pub segment_no: i16,
    /// `vendor.package.namespace.type`.
    pub segment_name: String,
    pub major: i64,
    pub minor: Option<i64>,
    /// The segment ends with `~`.
    pub is_type: bool,
}

/// No relations declared — see the note on [`super::version_family`].
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
