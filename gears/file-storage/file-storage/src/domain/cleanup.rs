//! Cleanup engine: orphan reconciliation and retention-policy expiry.
//!
//! `CleanupEngine::run_sweep` runs one best-effort cycle: a failing step is logged at `warn`
//! and does not abort the rest. There is no cross-instance coordination: sweeps may run on
//! every instance and are safe to repeat or overlap, because deletes are no-ops once the row
//! is gone and audit rows are written transactionally only when a row is actually deleted.

#![allow(unknown_lints, de0309_must_have_domain_model)]

use std::sync::Arc;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::audit::{AuditEntry, AuditOperation, AuditOutcome, FileEvent};
use crate::domain::multipart::MultipartUploadSession;
use crate::domain::policy::RetentionScope;
use crate::domain::ports::CleanupStore;
use crate::infra::backend::BackendRegistry;
use crate::infra::external_clients::{UsageDelta, UsageReporter};

/// Page size for the keyset-paginated retention file scan (bounds memory use).
const RETENTION_SWEEP_BATCH: u64 = 500;

/// Configuration knobs for the cleanup engine.
#[derive(Debug, Clone)]
pub struct CleanupConfig {
    /// Pending versions older than this many seconds are eligible for orphan reconciliation.
    pub orphan_grace_secs: u64,
}

/// Tally of what a single sweep cycle reconciled.
#[derive(Debug, Default, Clone)]
pub struct SweepResult {
    /// Number of abandoned pending version rows deleted (and their blobs).
    pub abandoned_pending_deleted: usize,
    /// Number of zero-version orphan `files` rows deleted after their last abandoned
    /// pending version was reclaimed.
    pub abandoned_files_deleted: usize,
    /// Number of expired in-progress multipart sessions aborted.
    pub expired_multipart_aborted: usize,
    /// Number of files deleted because a retention rule triggered.
    pub retention_expired_deleted: usize,
    /// Number of expired `idempotency_keys` rows deleted.
    pub idempotency_keys_deleted: u64,
}

/// The cleanup engine: orchestrates one cleanup sweep.
///
/// Call `run_sweep()` to execute one full cycle. The gear runs no background loop: a
/// separate cleanup job is expected to call this. Backend blob-without-row reconciliation
/// (enumerating via `list_paths`) is not done, as it would need cross-instance coordination.
pub struct CleanupEngine {
    store: Arc<dyn CleanupStore>,
    backends: BackendRegistry,
    config: CleanupConfig,
    /// Usage-reporting sink; `None` disables reporting.
    usage_reporter: Option<Arc<dyn UsageReporter>>,
}

impl CleanupEngine {
    #[must_use]
    pub fn new(
        store: Arc<dyn CleanupStore>,
        backends: BackendRegistry,
        config: CleanupConfig,
    ) -> Self {
        Self {
            store,
            backends,
            config,
            usage_reporter: None,
        }
    }

    /// Install a usage-reporting sink (builder step, so `new()` call sites stay unchanged).
    #[must_use]
    pub fn with_usage_reporter(mut self, usage_reporter: Option<Arc<dyn UsageReporter>>) -> Self {
        self.usage_reporter = usage_reporter;
        self
    }

    /// Fire-and-forget usage delta report; a failing reporter never blocks the sweep.
    fn report_usage(&self, delta: UsageDelta) {
        if let Some(reporter) = self.usage_reporter.clone() {
            tokio::spawn(async move {
                reporter.report(delta).await;
            });
        }
    }

    /// Run one sweep cycle (also directly callable for tests and admin use).
    ///
    /// Steps, each best-effort:
    /// 1. Abandoned pending versions older than the orphan grace window, except a version
    ///    backing a live `in_progress` multipart session (`expires_at > now`).
    /// 2. Expired multipart sessions (`expires_at < now`, still `in_progress`).
    /// 3. Retention-policy expiry (age / inactivity / metadata rules, all scopes).
    /// 4. Expired idempotency keys (`expires_at <= now`).
    ///
    /// Concurrent sweeps are safe: the first writer wins and the rest get `Ok(false)` from
    /// the delete methods.
    #[tracing::instrument(skip_all)]
    pub async fn run_sweep(&self) -> SweepResult {
        let mut result = SweepResult::default();
        let now = OffsetDateTime::now_utc();
        let grace =
            time::Duration::seconds(i64::try_from(self.config.orphan_grace_secs).unwrap_or(3600));
        let grace_cutoff = now - grace;

        let (pending_deleted, files_deleted) =
            self.sweep_abandoned_pending(grace_cutoff, now).await;
        result.abandoned_pending_deleted += pending_deleted;
        result.abandoned_files_deleted += files_deleted;

        result.expired_multipart_aborted += self.sweep_expired_multipart(now).await;

        result.retention_expired_deleted += self.sweep_retention_expiry(now).await;

        // `audit_outbox`/`events_outbox` are deliberately not purged: `published_at` stays
        // `NULL` until an event relay exists, so an age-based purge would drop undelivered rows.
        result.idempotency_keys_deleted += self
            .store
            .delete_expired_idempotency_keys(now)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = ?e, "cleanup: failed to delete expired idempotency keys");
                0
            });

        result
    }

    /// Delete pending versions never finalised and older than `grace_cutoff`; blob cleanup is
    /// best-effort.
    ///
    /// A pending version backing a live `in_progress` multipart session (`expires_at > now`)
    /// is never selected, whatever its age (see `CleanupStore::list_abandoned_pending_versions`).
    /// `now` is passed in so the guard uses the same instant the caller used, not one
    /// re-sampled in the query layer.
    ///
    /// Returns `(pending_versions_deleted, orphan_files_deleted)`.
    async fn sweep_abandoned_pending(
        &self,
        grace_cutoff: OffsetDateTime,
        now: OffsetDateTime,
    ) -> (usize, usize) {
        let versions = match self
            .store
            .list_abandoned_pending_versions(grace_cutoff, now)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "cleanup: failed to list abandoned pending versions"
                );
                return (0, 0);
            }
        };

        let mut pending_count = 0_usize;
        let mut files_count = 0_usize;
        for v in versions {
            let (pending, files) = self
                .delete_abandoned_pending_version(
                    v.file_id,
                    v.version_id,
                    v.size,
                    &v.backend_id,
                    &v.backend_path,
                )
                .await;
            pending_count += pending;
            files_count += files;
        }
        (pending_count, files_count)
    }

    /// Best-effort file lookup for audit tenant attribution; a failure is logged and treated as
    /// absent (nil tenant) rather than blocking reclamation.
    async fn load_file_for_audit(&self, file_id: Uuid) -> Option<file_storage_sdk::File> {
        match self.store.get_file(file_id).await {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    file_id = %file_id,
                    "cleanup: failed to load file for audit tenant attribution"
                );
                None
            }
        }
    }

    /// Delete one abandoned pending version row, best-effort delete its backend blob, and if
    /// that leaves the file with no versions and a `NULL` `content_id`, delete the file too.
    ///
    /// `size` is the version's `file_versions.size`, in practice `0` (only `finalize_version`
    /// sets it), but reported rather than hardcoded so the debit stays correct regardless.
    ///
    /// Returns `(pending_versions_deleted, orphan_files_deleted)`, each `0` or `1`.
    async fn delete_abandoned_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        backend_id: &str,
        backend_path: &str,
    ) -> (usize, usize) {
        let file = self.load_file_for_audit(file_id).await;
        let audit = AuditEntry {
            tenant_id: file.as_ref().map_or_else(Uuid::nil, |file| file.tenant_id),
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
                // `file_count_delta` is `0`: only the version row is gone here, the file's own
                // debit (if any) comes from `maybe_delete_orphaned_file`.
                if let Some(file) = file.as_ref() {
                    self.report_usage(UsageDelta {
                        tenant_id: file.tenant_id,
                        owner_id: file.owner_id,
                        bytes_delta: -size,
                        file_count_delta: 0,
                    });
                }

                // Row first, blob after: a failed blob delete only leaves an unreachable blob.
                self.best_effort_delete(backend_id, backend_path).await;
                let files_deleted = self.maybe_delete_orphaned_file(file_id).await;
                (1, files_deleted)
            }
            Ok(false) => {
                // Already removed by a concurrent sweep.
                (0, 0)
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    %version_id,
                    "cleanup: failed to delete abandoned pending version"
                );
                (0, 0)
            }
        }
    }

    /// After deleting a file's last abandoned pending version, delete the parent `files` row if
    /// it is now an orphan (no versions **and** `content_id IS NULL`). Returns `1` if deleted.
    ///
    /// The checks here are a pre-transaction pre-filter. The authoritative guard re-runs them
    /// inside the delete's transaction (`CleanupStore::delete_orphan_file_with_event`), so a
    /// version inserted or bound in the gap makes the delete abort instead of losing data.
    async fn maybe_delete_orphaned_file(&self, file_id: Uuid) -> usize {
        let Some(file) = self.orphan_candidate_file(file_id).await else {
            return 0;
        };

        let audit = orphan_reconcile_audit(
            file_id,
            file.tenant_id,
            serde_json::json!({
                "reason": "abandoned_pending_version_orphan_file",
            }),
        );
        let event = Some(FileEvent {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            file_id: file.file_id,
            event_type: "file.deleted".to_owned(),
            payload: serde_json::json!({
                "reason": "abandoned_pending_version_orphan_file",
            }),
        });

        match self
            .store
            .delete_orphan_file_with_event(file_id, audit, event)
            .await
        {
            Ok(true) => {
                // Debit the file count only: a zero-version file never had bytes credited.
                self.report_usage(UsageDelta {
                    tenant_id: file.tenant_id,
                    owner_id: file.owner_id,
                    bytes_delta: 0,
                    file_count_delta: -1,
                });
                1
            }
            Ok(false) => {
                // In-transaction guard failed (a version now exists) or already removed.
                0
            }
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    "cleanup: failed to delete orphaned zero-version file"
                );
                0
            }
        }
    }

    /// Pre-transaction check that `file_id` looks like a zero-version orphan (no versions,
    /// `NULL` `content_id`, no blocking multipart session). Returns the file if so, `None`
    /// if not (or on a logged lookup failure).
    async fn orphan_candidate_file(&self, file_id: Uuid) -> Option<file_storage_sdk::File> {
        let remaining = match self.store.list_versions(file_id).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    "cleanup: failed to list versions while checking for orphaned file"
                );
                return None;
            }
        };
        if !remaining.is_empty() {
            return None;
        }

        let file = match self.store.get_file(file_id).await {
            Ok(Some(f)) => f,
            Ok(None) => return None, // Already gone.
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    "cleanup: failed to fetch file while checking for orphaned file"
                );
                return None;
            }
        };
        if file.content_id.is_some() {
            // Bound content means a version exists; the `remaining` snapshot was stale.
            return None;
        }

        if self.has_blocking_multipart_session(file_id).await {
            return None;
        }

        Some(file)
    }

    /// Whether `file_id` has a not-yet-expired multipart session that blocks orphan-file
    /// deletion.
    ///
    /// Step 1 keys only on a pending version's age, so a live session can have its backing
    /// version reclaimed in the same pass. Deleting the file then would cascade
    /// (`ON DELETE CASCADE`) to the `in_progress` `multipart_uploads` row and silently destroy
    /// the upload. Blocking leaves the file for a later pass, after the session is
    /// aborted or completed. A lookup failure counts as blocking (errs toward not deleting).
    async fn has_blocking_multipart_session(&self, file_id: Uuid) -> bool {
        match self.store.has_in_progress_multipart_for_file(file_id).await {
            Ok(blocking) => blocking,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    "cleanup: failed to check in-progress multipart sessions while \
                     checking for orphaned file"
                );
                true
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

    /// Abort one expired multipart session: win the `in_progress -> aborted` CAS *first*, and
    /// only then clean up the backend handle and pending version.
    ///
    /// A concurrent `complete_multipart_upload` races on the same session-row CAS and only
    /// one wins. If the sweep loses (`Ok(false)`), the version may already be bound by the
    /// complete and must be left untouched.
    async fn abort_expired_multipart_session(&self, session: MultipartUploadSession) -> usize {
        let audit_tenant_id = self
            .store
            .get_file(session.file_id)
            .await
            .ok()
            .flatten()
            .map_or_else(Uuid::nil, |file| file.tenant_id);
        let abort_audit = AuditEntry {
            tenant_id: audit_tenant_id,
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
            Ok(true) => {
                // Won the CAS: no concurrent complete can bind this version now.
                self.cleanup_expired_session_version(&session).await;
                1
            }
            Ok(false) => {
                // A concurrent complete/abort won; after a complete the version is bound.
                tracing::info!(
                    upload_id = %session.upload_id,
                    "cleanup: skipping version cleanup, session no longer in_progress \
                     (concurrent complete/abort won the race)"
                );
                0
            }
            Err(e) => {
                tracing::warn!(error = ?e, upload_id = %session.upload_id,
                    "cleanup: failed to mark expired multipart upload as aborted");
                0
            }
        }
    }

    /// Abort the backend upload and delete the pending version row of an expired session; both
    /// best-effort.
    ///
    /// `pub` only so a unit test can drive the narrow interleaving window deterministically;
    /// otherwise called only after `abort_expired_multipart_session` has won the session CAS.
    pub async fn cleanup_expired_session_version(&self, session: &MultipartUploadSession) {
        let Ok(Some(ver)) = self
            .store
            .get_version(session.file_id, session.version_id)
            .await
        else {
            return;
        };

        self.backend_abort_multipart_best_effort(
            &ver.backend_id,
            &ver.backend_path,
            &session.backend_upload_handle,
            session.upload_id,
        )
        .await;

        // Status-guarded delete: if a racing `complete_multipart_upload` already flipped the
        // version to `available`, the DELETE matches zero rows.
        let del_audit = orphan_reconcile_audit(
            session.file_id,
            self.store
                .get_file(session.file_id)
                .await
                .ok()
                .flatten()
                .map_or_else(Uuid::nil, |file| file.tenant_id),
            serde_json::json!({
                "reason": "expired_multipart_version_cleanup",
                "upload_id": session.upload_id,
                "version_id": session.version_id,
            }),
        );
        if let Err(e) = self
            .store
            .delete_pending_version(session.file_id, session.version_id, del_audit)
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

    /// Delete files expired by a retention rule, scanning files in keyset-paginated batches
    /// (by `file_id`) so memory stays bounded. Rules are fetched once and reused.
    async fn sweep_retention_expiry(&self, now: OffsetDateTime) -> usize {
        let all_rules = match self.store.list_all_retention_rules().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = ?e, "cleanup: failed to list retention rules");
                return 0;
            }
        };
        // No rules: skip the file scan.
        if all_rules.is_empty() {
            return 0;
        }

        let mut count = 0_usize;
        let mut after: Option<Uuid> = None;
        // Keyset loop: the next page filters `file_id > after`, so deletions never shift the
        // window. A short page (or a query error) ends the sweep.
        while let Some(batch) = self.next_retention_page(after).await {
            if batch.is_empty() {
                break;
            }
            after = batch.last().map(|f| f.file_id);
            let last_page = (batch.len() as u64) < RETENTION_SWEEP_BATCH;
            count += self.expire_batch(&batch, &all_rules, now).await;
            if last_page {
                break;
            }
        }
        count
    }

    /// Next keyset page of files; `None` (logged) on a query error, ending the sweep.
    async fn next_retention_page(
        &self,
        after: Option<Uuid>,
    ) -> Option<Vec<file_storage_sdk::File>> {
        match self
            .store
            .list_all_files_for_sweep(after, RETENTION_SWEEP_BATCH)
            .await
        {
            Ok(files) => Some(files),
            Err(e) => {
                tracing::warn!(error = ?e, "cleanup: failed to list files for retention sweep");
                None
            }
        }
    }

    /// Apply retention rules to one page of files. Returns the number deleted.
    async fn expire_batch(
        &self,
        batch: &[file_storage_sdk::File],
        all_rules: &[crate::domain::policy::StoredRetentionRule],
        now: OffsetDateTime,
    ) -> usize {
        let mut count = 0_usize;
        for file in batch {
            count += self.maybe_expire_file(file, all_rules, now).await;
        }
        count
    }

    /// Apply retention rules to one file. Returns 1 if deleted, 0 otherwise.
    async fn maybe_expire_file(
        &self,
        file: &file_storage_sdk::File,
        all_rules: &[crate::domain::policy::StoredRetentionRule],
        now: OffsetDateTime,
    ) -> usize {
        let applicable: Vec<&crate::domain::policy::StoredRetentionRule> = all_rules
            .iter()
            .filter(|r| rule_applies_to_file(r, file))
            .collect();

        if applicable.is_empty() {
            return 0;
        }

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

    /// Versions of a file ahead of retention deletion. `None` (logged) on a store error, so
    /// the file is skipped rather than deleted as if it had zero versions.
    async fn list_versions_for_expiry(
        &self,
        file_id: Uuid,
    ) -> Option<Vec<file_storage_sdk::FileVersion>> {
        match self.store.list_versions(file_id).await {
            Ok(v) => Some(v),
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    file_id = %file_id,
                    "cleanup: failed to list versions for retention-expired file; skipping expiry"
                );
                None
            }
        }
    }

    /// Delete one retention-expired file (DB row + backend blobs). Returns 1 if deleted.
    async fn expire_file(&self, file: &file_storage_sdk::File, now: OffsetDateTime) -> usize {
        // Collect blob locations first; they are deleted after the DB row, never before.
        let Some(versions) = self.list_versions_for_expiry(file.file_id).await else {
            return 0;
        };

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

        // Emit `file.deleted` like user-initiated deletes; plain `delete_file` skips the event.
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
                // Debit the whole file, as the user-initiated delete path does.
                let total_bytes: i64 = versions.iter().map(|v| v.size).sum();
                self.report_usage(UsageDelta {
                    tenant_id: file.tenant_id,
                    owner_id: file.owner_id,
                    bytes_delta: -total_bytes,
                    file_count_delta: -1,
                });

                for v in &versions {
                    self.best_effort_delete(&v.backend_id, &v.backend_path)
                        .await;
                }
                1
            }
            Ok(false) => {
                // Already deleted by a concurrent sweep.
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

    /// Delete a blob from a backend; errors are logged, not propagated.
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

/// Build a system-actor `OrphanReconcile` audit entry.
fn orphan_reconcile_audit(file_id: Uuid, tenant_id: Uuid, detail: serde_json::Value) -> AuditEntry {
    AuditEntry {
        tenant_id,
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

/// Whether `body` triggers expiry for `file` (OR across criteria).
fn rule_matches(
    body: &crate::domain::policy::RetentionRuleBody,
    file: &file_storage_sdk::File,
    metadata: &[file_storage_sdk::CustomMetadataEntry],
    now: OffsetDateTime,
) -> bool {
    if let Some(age) = &body.age {
        let max_age = time::Duration::days(i64::from(age.max_age_days));
        if now - file.created_at > max_age {
            return true;
        }
    }

    if let Some(inact) = &body.inactivity {
        let inact_dur = time::Duration::days(i64::from(inact.inactivity_days));
        if now - file.last_modified_at > inact_dur {
            return true;
        }
    }

    if let Some(meta_rule) = &body.metadata
        && metadata
            .iter()
            .any(|e| e.key == meta_rule.key && e.value == meta_rule.value)
    {
        return true;
    }

    false
}
