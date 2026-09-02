//! `types_registry__operation_item` — one durable candidate and public result per
//! exact GTS Identifier within an operation (ADR-0012).
//!
//! Mirror of the table in `docs/database.sql`.
//!
//! `dry_run` and `kind` are **copies** of the parent's, because
//! `ck_tr_operation_item_state` cannot read another table.
//! `fk_tr_operation_item_operation` is a composite key onto
//! `(id, kind, dry_run)`, which is what keeps the copies in step.
//!
//! `expected_resource_version` encodes the closed precondition vocabulary: 0
//! means *must not exist*; `>= 1` is the version to match. On the wire an absent
//! field means must-not-exist and a literal 0 is rejected.
//!
//! `request_payload` is dropped at terminality. Failed and dry-run receipts keep
//! the structured outcome but not the submitted content, because keeping it would
//! retain rejected content for the lifetime of unrelated successful revisions.

use sea_orm::entity::prelude::*;
use time::OffsetDateTime;
use toolkit_db_macros::Scopable;
use uuid::Uuid;

use super::enums::{OperationItemStatus, OperationKind};

// ponytail: ceiling C6 — no PDP. This row carries no tenant column at all; its
// plane and tenant live on the parent operation, reached through `operation_id`.
// The upgrade path is the parent's: a scoped read of `operation` plus
// `PolicyEnforcer` once the identity-to-permission binding lands (SPEC §9 C6,
// §12).
#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Scopable)]
#[sea_orm(table_name = "types_registry__operation_item")]
#[secure(unrestricted)]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub operation_id: Uuid,
    pub item_no: i32,
    pub gts_id: String,
    /// Copied from `operation.dry_run`; held in step by the composite FK.
    pub dry_run: bool,
    /// Copied from `operation.kind`; held in step by the composite FK.
    pub kind: OperationKind,
    /// 0 means the candidate must not exist; otherwise the version to match.
    pub expected_resource_version: i64,
    pub status: OperationItemStatus,
    /// Dropped at terminality — NULL for every terminal status.
    pub request_payload: Option<String>,
    /// Written only by a committed, changed registration. Write-once.
    pub result_revision_no: Option<i32>,
    /// Every committed result has one; dry-run `Unchanged` reports the version it
    /// read, dry-run `Succeeded` allocates none. Write-once.
    pub result_resource_version: Option<i64>,
    pub error_payload: Option<String>,
    pub created_at: OffsetDateTime,
    pub started_at: Option<OffsetDateTime>,
    pub completed_at: Option<OffsetDateTime>,
}

/// No relations declared — see the note on [`super::version_family`].
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
