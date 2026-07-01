//! Repository for the `audit_outbox` table.
//!
//! All writes use `allow_all()` scope — the outbox has no `tenant_id` secure
//! column; the tenant identifier is instead stored as a plain data column and
//! enforced at the application level (the `Store` always writes the caller's
//! tenant).
//!
//! The `insert` method is designed to be called **inside an open transaction**
//! so the audit row is committed atomically with the mutation it describes.
//!
//! @cpt-cf-file-storage-fr-audit-trail
//! @cpt-cf-file-storage-nfr-audit-completeness

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use toolkit_db::secure::{DBRunner, SecureEntityExt, secure_insert};
use toolkit_security::AccessScope;
use uuid::Uuid;

use crate::domain::audit::AuditEntry;
use crate::domain::error::DomainError;
use crate::infra::storage::db::db_err;
use crate::infra::storage::entity::audit_outbox::{ActiveModel, Column, Entity};

/// Repository over the `audit_outbox` table.
#[derive(Clone, Default)]
pub struct AuditRepo;

impl AuditRepo {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Insert one audit row into `conn` (which may be a transaction reference).
    ///
    /// Callers MUST pass a transaction runner so the row is committed with the
    /// surrounding mutation (the atomicity invariant).
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
    /// @cpt-cf-file-storage-nfr-audit-completeness
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
        // No tenant scope on this table — allow_all() is intentional.
        secure_insert::<Entity>(am, &AccessScope::allow_all(), conn)
            .await
            .map_err(db_err)?;
        Ok(())
    }

    /// List unpublished audit rows for a specific file — useful in tests to
    /// verify that exactly the right rows were written.
    ///
    /// @cpt-cf-file-storage-fr-audit-trail
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
            .map_err(db_err)?;
        Ok(rows)
    }
}
