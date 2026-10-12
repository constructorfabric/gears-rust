//! Repository for the `audit_outbox` table.
//!
//! All writes use `allow_all()` scope: the outbox has no secure tenant column;
//! `tenant_id` is a plain data column and the `Store` always writes the caller's.
//!
//! `insert` must be called **inside an open transaction** so the audit row commits
//! atomically with the mutation it describes.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use toolkit_db::secure::{DBRunner, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::audit::AuditEntry;
use crate::domain::error::DomainError;
use crate::infra::storage::entity::audit_outbox::{ActiveModel, Column, Entity};

/// Repository over the `audit_outbox` table.
#[derive(Clone, Default)]
pub struct AuditRepo;

impl AuditRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Insert one audit row; `conn` MUST be the surrounding transaction.
    pub async fn insert<C: DBRunner>(
        &self,
        conn: &C,
        entry: &AuditEntry,
    ) -> Result<(), DomainError> {
        let am = ActiveModel {
            event_id: Set(Uuid::now_v7()),
            tenant_id: Set(entry.tenant_id),
            actor_kind: Set(entry.actor_kind.clone()),
            actor_id: Set(entry.actor_id),
            file_id: Set(entry.file_id),
            operation: Set(entry.operation.as_str().to_owned()),
            outcome: Set(entry.outcome.as_str().to_owned()),
            detail: Set(entry.detail.clone()),
            occurred_at: Set(entry.occurred_at),
            published_at: Set(None),
        };
        secure_insert::<Entity>(am, &AccessScope::allow_all(), conn)
            .await
            .map_err(DomainError::from)?;
        Ok(())
    }

    /// List unpublished audit rows for a file (used in tests).
    pub async fn list_for_file<C: DBRunner>(
        &self,
        conn: &C,
        file_id: Uuid,
    ) -> Result<Vec<crate::infra::storage::entity::audit_outbox::Model>, DomainError> {
        let rows = Entity::find()
            .filter(Column::FileId.eq(file_id))
            .order_by_asc(Column::OccurredAt)
            .secure()
            .scope_with(&AccessScope::allow_all())
            .all(conn)
            .await
            .map_err(DomainError::from)?;
        Ok(rows)
    }
}
