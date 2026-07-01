//! Background lifecycle & cleanup engine -- orphan reconciliation, retention-policy
//! expiry, and per-instance sweep scheduling.
//!
//! `CleanupEngine::run_sweep` is the single entry point for the cleanup cycle.
//! It is intentionally best-effort: one step's failure does not abort the rest.
//! Errors are logged at `warn` level rather than propagated.
//!
//! **No cross-instance coordination in P2.** The sweep runs independently on
//! every control-plane instance. Because all operations are idempotent (delete
//! is no-op when the row is already gone; audit rows are inserted transactionally
//! only when a row is deleted) concurrent sweeps on the same data are safe, just
//! redundant. Leader election / distributed locking is deferred to P3.
//!
//! @cpt-cf-file-storage-fr-orphan-reconciliation
//! @cpt-cf-file-storage-fr-retention-policies

#![allow(unknown_lints, de0309_must_have_domain_model)]

use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::audit::{AuditEntry, AuditOperation, AuditOutcome};
use crate::domain::multipart::MultipartUploadSession;
use crate::domain::policy::RetentionScope;
use crate::infra::backend::BackendRegistry;
use crate::infra::storage::Store;
use crate::infra::storage::repo::FileEvent;

/// Configuration knobs for the cleanup engine.
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// Pending versions / abandoned multipart sessions older than this many
    /// seconds are eligible for orphan reconciliation.
    pub orphan_grace_secs: u64,
}

/// Tally of what a single sweep cycle reconciled.
#[derive(Debug, Default, Clone)]
pub struct SweepResult {
    /// Number of abandoned pending version rows deleted (and their blobs).
    pub abandoned_pending_deleted: usize,
    /// Number of expired in-progress multipart sessions aborted.
    pub expired_multipart_aborted: usize,
    /// Number of files deleted because a retention rule triggered.
    pub retention_expired_deleted: usize,
}

/// The cleanup engine -- orchestrates the background sweep.
///
/// Call `run_sweep()` to execute one full cycle. The gear wires a repeating
/// `tokio::time::sleep` loop that calls this when `enable_background_sweep` is
/// `true`.
///
/// **P2 scope**: orphan reconciliation + retention-policy expiry.
/// Backend blob-without-row reconciliation (cross-backend orphan enumeration via
/// `list_paths`) requires cross-instance leader election to be safe and is
/// therefore deferred to P3.
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
/// @cpt-cf-file-storage-fr-retention-policies
pub struct CleanupEngine {
    store: Store,
    backends: BackendRegistry,
    config: CleanupConfig,
}

impl CleanupEngine {
    /// Create a new `CleanupEngine`.
    #[must_use]
    pub fn new(store: Store, backends: BackendRegistry, config: CleanupConfig) -> Self {
        Self {
            store,
            backends,
            config,
        }
    }

    /// Run one sweep cycle. Directly callable for testing and admin use.
    ///
    /// Sweep order (each step is best-effort -- one failure does not abort the
    /// rest):
    /// 1. Abandoned pending versions (pre-registered but never finalised, past
    ///    the orphan grace window).
    /// 2. Expired multipart sessions (`expires_at < now`, still `in_progress`).
    /// 3. Retention-policy expiry (age / inactivity / metadata rules, all scopes).
    ///
    /// Cross-instance coordination is deliberately absent in P2. The sweep is
    /// idempotent: concurrent sweeps on the same data produce at most one
    /// successful deletion per row (the first writer wins; the rest get
    /// `Ok(false)` from the version/file delete methods).
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    /// @cpt-cf-file-storage-fr-retention-policies
    pub async fn run_sweep(&self) -> SweepResult {
        let mut result = SweepResult::default();
        let now = OffsetDateTime::now_utc();
        let grace =
            time::Duration::seconds(i64::try_from(self.config.orphan_grace_secs).unwrap_or(3600));
        let grace_cutoff = now - grace;

        // Step 1 -- abandoned pending versions.
        result.abandoned_pending_deleted += self.sweep_abandoned_pending(grace_cutoff).await;

        // Step 2 -- expired multipart sessions.
        result.expired_multipart_aborted += self.sweep_expired_multipart(now).await;

        // Step 3 -- retention-policy expiry.
        result.retention_expired_deleted += self.sweep_retention_expiry(now).await;

        result
    }

    // ── private sweep methods ──────────────────────────────────────────────────

    /// Delete pending version rows that were never finalised and are older than
    /// `grace_cutoff`. Blob bytes are cleaned up on a best-effort basis.
    async fn sweep_abandoned_pending(&self, grace_cutoff: OffsetDateTime) -> usize {
        let versions = match self
            .store
            .list_abandoned_pending_versions(grace_cutoff)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "cleanup: failed to list abandoned pending versions"
                );
                return 0;
            }
        };

        let mut count = 0_usize;
        for v in versions {
            count += self
                .delete_abandoned_pending_version(
                    v.file_id,
                    v.version_id,
                    &v.backend_id,
                    &v.backend_path,
                )
                .await;
        }
        count
    }

    /// Delete one abandoned pending version row and clean up its backend blob.
    async fn delete_abandoned_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        backend_id: &str,
        backend_path: &str,
    ) -> usize {
        let audit = AuditEntry {
            tenant_id: Uuid::nil(),
            actor_kind: "system".to_owned(),
            actor_id: Uuid::nil(),
            file_id: Some(file_id),
            operation: AuditOperation::OrphanReconcile,
            outcome: AuditOutcome::Success,
            detail: serde_json::json!({
                "reason": "abandoned_pending_version",
                "version_id": version_id,
            }),
            occurred_at: OffsetDateTime::now_utc(),
        };
        match self.store.delete_version(file_id, version_id, audit).await {
            Ok(true) => {
                // Best-effort blob cleanup -- a failure here leaves an unreachable
                // orphan blob which is acceptable in P2.
                self.best_effort_delete(backend_id, backend_path).await;
                1
            }
            Ok(false) => {
                // Already removed by a concurrent sweep -- fine.
                0
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    %version_id,
                    "cleanup: failed to delete abandoned pending version"
                );
                0
            }
        }
    }

    /// Abort in-progress multipart sessions whose `expires_at` has passed.
    async fn sweep_expired_multipart(&self, now: OffsetDateTime) -> usize {
        let sessions = match self.store.list_expired_multipart_uploads(now).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "cleanup: failed to list expired multipart uploads"
                );
                return 0;
            }
        };

        let mut count = 0_usize;
        for session in sessions {
            count += self.abort_expired_multipart_session(session).await;
        }
        count
    }

    /// Abort one expired multipart session: tell the backend to drop the
    /// in-progress upload, delete the pending version row, and mark the session
    /// as aborted.
    async fn abort_expired_multipart_session(&self, session: MultipartUploadSession) -> usize {
        // Clean up the backend upload handle and pending version row.
        self.cleanup_expired_session_version(&session).await;

        // Mark the multipart session itself as aborted.
        let abort_audit = AuditEntry {
            tenant_id: Uuid::nil(),
            actor_kind: "system".to_owned(),
            actor_id: Uuid::nil(),
            file_id: Some(session.file_id),
            operation: AuditOperation::MultipartAbort,
            outcome: AuditOutcome::Success,
            detail: serde_json::json!({
                "reason": "expired_multipart_session_cleanup",
                "upload_id": session.upload_id,
            }),
            occurred_at: OffsetDateTime::now_utc(),
        };
        match self
            .store
            .abort_multipart_upload(session.upload_id, abort_audit)
            .await
        {
            Ok(_) => 1,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    upload_id = %session.upload_id,
                    "cleanup: failed to mark expired multipart upload as aborted"
                );
                0
            }
        }
    }

    /// Helper: abort the backend upload and delete the pending version row for
    /// an expired multipart session. Both operations are best-effort.
    async fn cleanup_expired_session_version(&self, session: &MultipartUploadSession) {
        let Ok(Some(ver)) = self
            .store
            .get_version(session.file_id, session.version_id)
            .await
        else {
            return;
        };

        // Best-effort: tell the backend to discard the in-progress upload.
        self.backend_abort_multipart_best_effort(
            &ver.backend_id,
            &ver.backend_path,
            &session.backend_upload_handle,
            session.upload_id,
        )
        .await;

        // Best-effort: delete the pending version row.
        let del_audit = orphan_reconcile_audit(
            session.file_id,
            serde_json::json!({
                "reason": "expired_multipart_version_cleanup",
                "upload_id": session.upload_id,
                "version_id": session.version_id,
            }),
        );
        if let Err(e) = self
            .store
            .delete_version(session.file_id, session.version_id, del_audit)
            .await
        {
            tracing::warn!(
                error = ?e,
                version_id = %session.version_id,
                "cleanup: failed to delete pending version for expired multipart"
            );
        }
    }

    /// Tell a backend to abort a multipart upload handle; log and ignore errors.
    async fn backend_abort_multipart_best_effort(
        &self,
        backend_id: &str,
        path: &str,
        handle: &str,
        upload_id: Uuid,
    ) {
        if let Ok(backend) = self.backends.get(backend_id)
            && let Err(e) = backend.abort_multipart(path, handle).await
        {
            tracing::warn!(
                error = ?e,
                %upload_id,
                "cleanup: backend abort_multipart failed (continuing)"
            );
        }
    }

    /// Delete files that have been expired by a retention rule.
    async fn sweep_retention_expiry(&self, now: OffsetDateTime) -> usize {
        // Fetch all files and all retention rules in bulk to avoid N+1 queries.
        let files = match self.store.list_all_files_for_sweep().await {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = ?e, "cleanup: failed to list files for retention sweep");
                return 0;
            }
        };

        let all_rules = match self.store.list_all_retention_rules().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = ?e, "cleanup: failed to list retention rules");
                return 0;
            }
        };

        let mut count = 0_usize;
        for file in &files {
            count += self.maybe_expire_file(file, &all_rules, now).await;
        }
        count
    }

    /// Check and apply retention rules to one file. Returns 1 if deleted, 0 otherwise.
    async fn maybe_expire_file(
        &self,
        file: &file_storage_sdk::File,
        all_rules: &[crate::domain::policy::StoredRetentionRule],
        now: OffsetDateTime,
    ) -> usize {
        // Gather applicable rules: tenant-scope, user-scope (owner), file-scope.
        let applicable: Vec<&crate::domain::policy::StoredRetentionRule> = all_rules
            .iter()
            .filter(|r| rule_applies_to_file(r, file))
            .collect();

        if applicable.is_empty() {
            return 0;
        }

        // Fetch custom metadata for metadata-criterion rules.
        let metadata = match self.store.list_metadata(file.file_id).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    file_id = %file.file_id,
                    "cleanup: failed to fetch metadata for retention check -- skipping file"
                );
                return 0;
            }
        };

        // OR semantics: if any rule triggers, delete the file.
        let should_expire = applicable
            .iter()
            .any(|r| rule_matches(&r.body, file, &metadata, now));

        if !should_expire {
            return 0;
        }

        self.expire_file(file, now).await
    }

    /// Delete one retention-expired file (DB row + backend blobs). Returns 1 if deleted.
    async fn expire_file(&self, file: &file_storage_sdk::File, now: OffsetDateTime) -> usize {
        // Collect version blobs before deleting so we can clean them up
        // after the DB row is gone.
        let versions = self
            .store
            .list_versions(file.file_id)
            .await
            .unwrap_or_default();

        let audit = AuditEntry {
            tenant_id: file.tenant_id,
            actor_kind: "system".to_owned(),
            actor_id: Uuid::nil(),
            file_id: Some(file.file_id),
            operation: AuditOperation::RetentionDelete,
            outcome: AuditOutcome::Success,
            detail: serde_json::json!({
                "reason": "retention_policy_expired",
                "file_id": file.file_id,
                "expired_at": now,
            }),
            occurred_at: now,
        };

        // Emit `file.deleted` on the same transactional-outbox path user-initiated
        // deletes use, so downstream consumers observe retention-driven deletions
        // too (a plain `delete_file` would silently skip the event).
        // @cpt-cf-file-storage-fr-file-events
        let event = Some(FileEvent {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            file_id: file.file_id,
            event_type: "file.deleted".to_owned(),
            payload: serde_json::json!({
                "reason": "retention_policy_expired",
                "expired_at": now,
            }),
        });

        let scope = toolkit_security::AccessScope::allow_all();
        match self
            .store
            .delete_file_with_event(&scope, file.file_id, audit, event)
            .await
        {
            Ok(true) => {
                for v in &versions {
                    self.best_effort_delete(&v.backend_id, &v.backend_path)
                        .await;
                }
                1
            }
            Ok(false) => {
                // Concurrent sweep already deleted it -- fine.
                0
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    file_id = %file.file_id,
                    "cleanup: failed to delete retention-expired file"
                );
                0
            }
        }
    }

    /// Delete a blob from a backend on a best-effort basis (errors are logged,
    /// not propagated).
    async fn best_effort_delete(&self, backend_id: &str, path: &str) {
        let Ok(backend) = self.backends.get(backend_id) else {
            tracing::warn!(
                backend_id,
                path,
                "cleanup: backend not found for best-effort delete"
            );
            return;
        };
        if let Err(e) = backend.delete(path).await {
            tracing::warn!(
                error = ?e,
                path,
                "cleanup: best-effort backend delete failed"
            );
        }
    }
}

// ── free helpers ──────────────────────────────────────────────────────────────

/// Build a system-actor `OrphanReconcile` audit entry.
fn orphan_reconcile_audit(file_id: Uuid, detail: serde_json::Value) -> AuditEntry {
    AuditEntry {
        tenant_id: Uuid::nil(),
        actor_kind: "system".to_owned(),
        actor_id: Uuid::nil(),
        file_id: Some(file_id),
        operation: AuditOperation::OrphanReconcile,
        outcome: AuditOutcome::Success,
        detail,
        occurred_at: OffsetDateTime::now_utc(),
    }
}

/// Return `true` when a retention rule applies to `file` based on its scope.
fn rule_applies_to_file(
    rule: &crate::domain::policy::StoredRetentionRule,
    file: &file_storage_sdk::File,
) -> bool {
    rule.tenant_id == file.tenant_id
        && match rule.scope {
            RetentionScope::Tenant => true,
            RetentionScope::User => rule.scope_target_id == Some(file.owner_id),
            RetentionScope::File => rule.scope_target_id == Some(file.file_id),
        }
}

/// Evaluate whether `body` triggers expiry for `file` given its custom
/// `metadata` and the current `now`.
///
/// OR semantics across criteria: the first matching criterion wins.
fn rule_matches(
    body: &crate::domain::policy::RetentionRuleBody,
    file: &file_storage_sdk::File,
    metadata: &[file_storage_sdk::CustomMetadataEntry],
    now: OffsetDateTime,
) -> bool {
    // Age-based: file created more than `max_age_days` ago.
    if let Some(age) = &body.age {
        let max_age = time::Duration::days(i64::from(age.max_age_days));
        if now - file.created_at > max_age {
            return true;
        }
    }

    // Inactivity-based: file not modified for `inactivity_days`.
    if let Some(inact) = &body.inactivity {
        let inact_dur = time::Duration::days(i64::from(inact.inactivity_days));
        if now - file.last_modified_at > inact_dur {
            return true;
        }
    }

    // Metadata-based: a specific key equals a specific value.
    if let Some(meta_rule) = &body.metadata
        && metadata
            .iter()
            .any(|e| e.key == meta_rule.key && e.value == meta_rule.value)
    {
        return true;
    }

    false
}
