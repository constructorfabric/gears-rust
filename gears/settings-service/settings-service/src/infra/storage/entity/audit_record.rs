// Created: 2026-09-07 by Virtuozzo International GmbH
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
    /// Record id.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    /// The resource the record is about (a declaration, category or value).
    pub resource: String,
    /// Key of the setting the record concerns.
    pub declaration_key: String,
    /// The scope the record is about, absent for a definition — a declaration
    /// or a category — which is platform-wide and sits at no scope. A scoped
    /// read cannot see such a row (`tenant_id = $1` and `tenant_id IN (…)` are
    /// both never true for NULL), which is correct: per-`(setting, scope)`
    /// history is about values, and these rows are not about a scope at all.
    /// The history read reaches them through an explicit `IS NULL` branch.
    pub tenant_id: Option<Uuid>,
    /// The operation performed, as the wire string of `AuditOperation` (`create`, `change`, `revert`, ...).
    pub operation: String,
    /// Subject that performed the operation.
    pub actor: String,
    /// Whether the actor identity is `public` or `pii`, as the wire string of `ActorClassification`.
    pub actor_classification: String,
    /// The value before the operation, if any.
    #[sea_orm(nullable)]
    pub pre_value: Option<Json>,
    /// The value after the operation, if any.
    #[sea_orm(nullable)]
    pub post_value: Option<Json>,
    /// Result of the operation: `success` or `failure`.
    pub outcome: String,
    /// Id of the request that caused the operation, for correlation.
    pub request_id: String,
    /// Id of the change set the record belongs to, when written as part of one.
    #[sea_orm(nullable)]
    pub change_set_id: Option<Uuid>,
    /// When the operation happened.
    pub occurred_at: OffsetDateTime,
    /// Earliest time the record may be purged; absent means no retention deadline yet.
    #[sea_orm(nullable)]
    pub retain_until: Option<OffsetDateTime>,
}

/// Relations of the `audit_record` entity (none).
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
