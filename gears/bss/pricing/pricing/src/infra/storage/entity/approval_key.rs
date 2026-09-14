//! `SeaORM` entity for `bss.pricing_approval_key` — the canonical scope keys a
//! `submitted` approval unit **holds** (`design/07-pricewindow-linkage.md`
//! `inst-co-single-pending`).
//!
//! One row per (unit, key). The set answers exactly one question — *"is this key
//! held by a pending unit?"* — and `PENDING_CHANGE_UNIT_EXISTS` is what reads it.
//!
//! `state` is denormalised from `pricing_approval` and is **not** written by this
//! crate after the insert: `trg_pricing_approval_key_follow_state` carries it, so
//! a unit that is decided, rejected or voided frees its keys without any decision
//! path having to remember to say so. The migration's module doc weighs that
//! against the two alternatives.
//!
//! There is no `approval_id` foreign key, and the chain declares them on its
//! sibling tables — so the absence is this table's own decision rather than a
//! house style. What stands in for it is the insert guard the migration's own doc
//! argues for: a row born under a missing or already-decided unit would hold its
//! key forever, because `follow_state` fires only `AFTER UPDATE` and the parent
//! refuses every UPDATE once decided. The composite primary key
//! `(approval_id, scope_key)` is what makes a unit's key set a set.

use sea_orm::entity::prelude::*;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "pricing_approval_key")]
#[secure(
    tenant_col = "tenant_id",
    resource_col = "approval_id",
    no_owner,
    no_type
)]
pub struct Model {
    /// The unit that holds the key.
    #[sea_orm(primary_key, auto_increment = false)]
    pub approval_id: Uuid,
    /// The key it holds, in the **canonical ten-axis rendering** — the same
    /// string a publish refusal names a key by, and the same
    /// `ScopeKey::to_string` produces. The migration's doc says why the axes are
    /// not spread into columns.
    #[sea_orm(primary_key, auto_increment = false)]
    pub scope_key: String,
    /// RLS scope.
    pub tenant_id: Uuid,
    /// `submitted` | `approved` | `rejected` | `voided` — the parent's state,
    /// mirrored so that `uq_pricing_approval_key_pending` can be partial on it.
    pub state: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
