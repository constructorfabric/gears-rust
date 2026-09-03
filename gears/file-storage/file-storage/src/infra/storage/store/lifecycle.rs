//! Lifecycle / cleanup / sweep intent methods and idempotency key queries.
//!
//! Covers: abandoned pending versions, expired multipart sessions, audit
//! outbox query, retention-rule sweep helpers, sweep file-list pagination,
//! and idempotency-key lookup.
//!
//! Superseded (non-current) version reclamation is **not** part of the P2
//! sweep -- see the "Superseded version retention" note in `DESIGN.md`
//! (§3.7, `file_versions` table) for the deferral rationale. It is deferred
//! to P3 pending a versioning-policy schema (e.g. `keep_last_n` /
//! `max_non_current_age_days`); no such field exists on `RetentionRuleBody`
//! today (`crate::domain::policy`).

use time::OffsetDateTime;
use toolkit_security::AccessScope;
use uuid::Uuid;

use file_storage_sdk::{File, FileVersion};

use crate::domain::error::DomainError;
use crate::domain::idempotency::IdempotencyRecord;
use crate::domain::multipart::MultipartUploadSession;
use crate::domain::policy::StoredRetentionRule;
use crate::infra::storage::db::db_err;
use crate::infra::storage::repo::AuditRow;
use crate::infra::storage::store::Store;

impl Store {
    // ── idempotency keys ──────────────────────────────────────────────────────

    /// Fetch an idempotency record if it exists and has not expired.
    pub async fn get_idempotency_key(
        &self,
        tenant_id: Uuid,
        owner_kind: &str,
        owner_id: Uuid,
        key: &str,
        now: OffsetDateTime,
    ) -> Result<Option<IdempotencyRecord>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .idempotency_keys
            .get(&conn, tenant_id, owner_kind, owner_id, key, now)
            .await
    }

    // ── audit outbox ──────────────────────────────────────────────────────────

    /// List audit rows for a specific file, ordered by occurrence time.
    ///
    /// Intended for testing; not exposed on the REST API.
    pub async fn list_audit(&self, file_id: Uuid) -> Result<Vec<AuditRow>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.audit.list_for_file(&conn, file_id).await
    }

    // ── cleanup engine ────────────────────────────────────────────────────────

    /// List all `pending` version rows older than `older_than` (system scope),
    /// excluding versions still backing a live `in_progress` multipart session
    /// (`expires_at > now`) -- see
    /// [`VersionRepo::list_pending_older_than`][crate::infra::storage::repo::VersionRepo::list_pending_older_than]
    /// for the invariant this protects.
    pub async fn list_abandoned_pending_versions(
        &self,
        older_than: OffsetDateTime,
        now: OffsetDateTime,
    ) -> Result<Vec<FileVersion>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .versions
            .list_pending_older_than(&conn, &AccessScope::allow_all(), older_than, now)
            .await
    }

    /// List all `in_progress` multipart sessions whose `expires_at` is before `now`.
    pub async fn list_expired_multipart_uploads(
        &self,
        now: OffsetDateTime,
    ) -> Result<Vec<MultipartUploadSession>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.multipart.list_expired(&conn, now).await
    }

    /// List files across all tenants for the retention sweep, keyset-paginated
    /// by `file_id` (see [`FileRepo::list_all_for_sweep`]). `after = None` starts
    /// from the beginning; the caller loops until it gets fewer than `limit`.
    pub async fn list_all_files_for_sweep(
        &self,
        after: Option<Uuid>,
        limit: u64,
    ) -> Result<Vec<File>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .files
            .list_all_for_sweep(&conn, &AccessScope::allow_all(), after, limit)
            .await
    }

    /// List retention rules for a specific file (`scope = 'file'`), across all
    /// tenants. Used by the retention sweep engine.
    pub async fn list_file_retention_rules(
        &self,
        file_id: Uuid,
    ) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .list_by_file_scope(&conn, &AccessScope::allow_all(), file_id)
            .await
    }

    /// List all retention rules across all tenants and scopes — for the sweep
    /// engine.
    pub async fn list_all_retention_rules(&self) -> Result<Vec<StoredRetentionRule>, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos
            .retention_rules
            .list_all(&conn, &AccessScope::allow_all())
            .await
    }

    /// Bulk-delete all `idempotency_keys` rows whose `expires_at` is at or
    /// before `now`. Returns the number of rows removed.
    pub async fn delete_expired_idempotency_keys(
        &self,
        now: OffsetDateTime,
    ) -> Result<u64, DomainError> {
        let conn = self.db.conn().map_err(db_err)?;
        self.repos.idempotency_keys.delete_expired(&conn, now).await
    }
}
