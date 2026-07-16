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

use std::sync::Arc;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::audit::{AuditEntry, AuditOperation, AuditOutcome, FileEvent};
use crate::domain::multipart::MultipartUploadSession;
use crate::domain::policy::RetentionScope;
use crate::domain::ports::CleanupStore;
use crate::infra::backend::BackendRegistry;
use crate::infra::external_clients::{UsageDelta, UsageReporter};

/// Page size for the keyset-paginated retention file scan. Bounds how many
/// `File` rows the sweep holds in memory at once, independent of total count.
const RETENTION_SWEEP_BATCH: u64 = 500;

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
    /// Number of permanent zero-version orphan `files` rows deleted after
    /// their last abandoned pending version was reclaimed (P2 2.8).
    pub abandoned_files_deleted: usize,
    /// Number of expired in-progress multipart sessions aborted.
    pub expired_multipart_aborted: usize,
    /// Number of files deleted because a retention rule triggered.
    pub retention_expired_deleted: usize,
    /// Number of expired `idempotency_keys` rows deleted.
    pub idempotency_keys_deleted: u64,
}

/// The cleanup engine -- orchestrates the background sweep.
///
/// Call `run_sweep()` to execute one full cycle. The gear lifecycle wires a
/// cancellable repeating sleep loop that calls this when
/// `enable_background_sweep` is `true`.
///
/// **P2 scope**: orphan reconciliation + retention-policy expiry.
/// Backend blob-without-row reconciliation (cross-backend orphan enumeration via
/// `list_paths`) requires cross-instance leader election to be safe and is
/// therefore deferred to P3.
///
/// @cpt-cf-file-storage-fr-orphan-reconciliation
/// @cpt-cf-file-storage-fr-retention-policies
pub struct CleanupEngine {
    store: Arc<dyn CleanupStore>,
    backends: BackendRegistry,
    config: CleanupConfig,
    /// Usage-reporting sink (P2 1.12 remediation). `None` disables reporting
    /// (fire-and-forget no-op); `gear.rs` opts in via
    /// [`Self::with_usage_reporter`] once a Usage Collector client is wired.
    usage_reporter: Option<Arc<dyn UsageReporter>>,
}

impl CleanupEngine {
    /// Create a new `CleanupEngine`.
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

    /// Install a usage-reporting sink (P2 1.12 remediation). Kept as a
    /// builder step (mirroring `FileService`/`MultipartService`'s
    /// `with_metrics`/`with_usage_reporter`) so existing `CleanupEngine::new(...)`
    /// call sites across the test suite keep compiling unchanged.
    #[must_use]
    pub fn with_usage_reporter(mut self, usage_reporter: Option<Arc<dyn UsageReporter>>) -> Self {
        self.usage_reporter = usage_reporter;
        self
    }

    /// Fire-and-forget usage delta report. Failures are logged but never
    /// propagated -- a failing usage reporter must not block the sweep.
    ///
    /// @cpt-cf-file-storage-fr-usage-reporting
    fn report_usage(&self, delta: UsageDelta) {
        if let Some(reporter) = self.usage_reporter.clone() {
            tokio::spawn(async move {
                reporter.report(delta).await;
            });
        }
    }

    /// Run one sweep cycle. Directly callable for testing and admin use.
    ///
    /// Sweep order (each step is best-effort -- one failure does not abort the
    /// rest):
    /// 1. Abandoned pending versions (pre-registered but never finalised, past
    ///    the orphan grace window) -- **except** a version still backing a
    ///    live `in_progress` multipart session (`expires_at > now`), which is
    ///    never selected regardless of age (P2 remediation 2.8).
    /// 2. Expired multipart sessions (`expires_at < now`, still `in_progress`).
    /// 3. Retention-policy expiry (age / inactivity / metadata rules, all scopes).
    /// 4. Expired idempotency-key rows (`expires_at <= now`). `audit_outbox`/
    ///    `events_outbox` rows are deliberately left untouched -- see the
    ///    inline comment at the call site.
    ///
    /// Cross-instance coordination is deliberately absent in P2. The sweep is
    /// idempotent: concurrent sweeps on the same data produce at most one
    /// successful deletion per row (the first writer wins; the rest get
    /// `Ok(false)` from the version/file delete methods).
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    /// @cpt-cf-file-storage-fr-retention-policies
    /// @cpt-dod:cpt-cf-file-storage-dod-cleanup-engine:p1
    #[tracing::instrument(skip_all)]
    pub async fn run_sweep(&self) -> SweepResult {
        let mut result = SweepResult::default();
        let now = OffsetDateTime::now_utc();
        let grace =
            time::Duration::seconds(i64::try_from(self.config.orphan_grace_secs).unwrap_or(3600));
        let grace_cutoff = now - grace;

        // @cpt-begin:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-best-effort
        // Step 1 -- abandoned pending versions (+ the parent `files` row, if
        // reclaiming the version leaves it a permanent zero-version orphan).
        // @cpt-begin:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-step1
        let (pending_deleted, files_deleted) =
            self.sweep_abandoned_pending(grace_cutoff, now).await;
        result.abandoned_pending_deleted += pending_deleted;
        result.abandoned_files_deleted += files_deleted;
        // @cpt-end:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-step1

        // Step 2 -- expired multipart sessions.
        // @cpt-begin:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-step2
        result.expired_multipart_aborted += self.sweep_expired_multipart(now).await;
        // @cpt-end:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-step2

        // Step 3 -- retention-policy expiry.
        // @cpt-begin:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-step3
        result.retention_expired_deleted += self.sweep_retention_expiry(now).await;
        // @cpt-end:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-step3

        // Step 4 -- expired idempotency-key rows (P2 remediation 1.9). The
        // `audit_outbox`/`events_outbox` tables are deliberately NOT swept
        // here: `published_at` stays `NULL` until the Tier 4 EventBroker
        // relay exists, so a row-age-based purge would silently drop rows
        // that were never delivered.
        // @cpt-begin:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-step4
        result.idempotency_keys_deleted += self
            .store
            .delete_expired_idempotency_keys(now)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = ?e, "cleanup: failed to delete expired idempotency keys");
                0
            });
        // @cpt-end:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-step4
        // @cpt-end:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-best-effort

        // @cpt-begin:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-return
        result
        // @cpt-end:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-return
    }

    // ── private sweep methods ──────────────────────────────────────────────────

    /// Delete pending version rows that were never finalised and are older than
    /// `grace_cutoff`. Blob bytes are cleaned up on a best-effort basis.
    ///
    /// Invariant: a pending version referenced by a live `in_progress`
    /// multipart session (`expires_at > now`) is never selected here,
    /// regardless of age -- see
    /// [`crate::domain::ports::CleanupStore::list_abandoned_pending_versions`].
    /// This is why `now` is threaded through alongside `grace_cutoff`: the
    /// guard must use the *same* "now" the caller used to decide the session
    /// is still live, not a value re-sampled inside the query layer.
    ///
    /// Returns `(pending_versions_deleted, orphan_files_deleted)`.
    // @cpt-begin:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-list
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
        // @cpt-end:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-list

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
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-return
        (pending_count, files_count)
        // @cpt-end:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-return
    }

    /// Best-effort load of a file row for audit tenant attribution. A failed
    /// lookup is logged and treated as absent, so the caller falls back to a
    /// nil tenant rather than blocking reclamation.
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

    /// Delete one abandoned pending version row, clean up its backend blob,
    /// and -- if that leaves the parent file with no versions and a `NULL`
    /// `content_id` -- delete the now-permanently-orphaned `files` row too
    /// (P2 2.8).
    ///
    /// `size` is the pending version's `file_versions.size` -- structurally
    /// `0` in practice, since a version is only ever assigned a nonzero size
    /// by `finalize_version`, and a version reclaimed here never reached
    /// that call. It is still read back and reported (rather than a
    /// hardcoded `0`) so this debit stays correct even if that invariant
    /// ever changes.
    ///
    /// The delete itself is status-guarded (`delete_pending_version`, only
    /// removes the row while it is still `status = pending`) -- the same CAS
    /// pattern `sweep_expired_multipart`'s step already uses. Between
    /// `list_abandoned_pending_versions` returning this row and this call
    /// running, a client's `finalize_upload` can race in and flip the version
    /// `pending -> available`; an unconditional delete would then remove a
    /// just-finalized version row (and its backend blob) out from under the
    /// caller. The guard makes that race a no-op here instead: `Ok(false)` is
    /// returned and neither the row, the blob, nor the reclaimed-bytes debit
    /// are touched.
    ///
    /// Returns `(pending_versions_deleted, orphan_files_deleted)`, each `0`
    /// or `1`.
    ///
    /// `pub` (rather than private) solely so a unit test can invoke it
    /// directly to exercise the narrow mid-flight interleaving window
    /// deterministically, without real concurrency -- mirroring why
    /// [`Self::cleanup_expired_session_version`] is `pub` for the same
    /// reason on step 2's sibling race. This function is otherwise only ever
    /// called from `sweep_abandoned_pending` with a snapshot straight out of
    /// `list_abandoned_pending_versions`.
    pub async fn delete_abandoned_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        backend_id: &str,
        backend_path: &str,
    ) -> (usize, usize) {
        let file = self.load_file_for_audit(file_id).await;
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-audit-delete
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
        match self
            .store
            .delete_pending_version(file_id, version_id, audit)
            .await
        {
            // @cpt-end:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-audit-delete
            Ok(true) => {
                // @cpt-cf-file-storage-fr-usage-reporting
                // @cpt-begin:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-usage
                // Debit the pending version's bytes; `file_count_delta` is
                // `0` because only the version row is gone here, not the
                // parent file (that follow-on debit, if any, is reported
                // separately by `maybe_delete_orphaned_file` below).
                // Best-effort: a failed file lookup just skips the (usually
                // zero-magnitude) report rather than blocking reclamation.
                if let Some(file) = file.as_ref() {
                    self.report_usage(UsageDelta {
                        tenant_id: file.tenant_id,
                        owner_id: file.owner_id,
                        bytes_delta: -size,
                        file_count_delta: 0,
                    });
                }
                // @cpt-end:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-usage

                // Best-effort blob cleanup -- a failure here leaves an unreachable
                // orphan blob which is acceptable in P2.
                // @cpt-begin:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-blob
                self.best_effort_delete(backend_id, backend_path).await;
                // @cpt-end:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-blob
                // @cpt-begin:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-orphan-file
                let files_deleted = self.maybe_delete_orphaned_file(file_id).await;
                // @cpt-end:cpt-cf-file-storage-algo-sweep-abandoned-pending:p1:inst-sweep-pending-orphan-file
                (1, files_deleted)
            }
            Ok(false) => {
                // Either already removed by a concurrent sweep, or -- the
                // race this guard exists for -- a client's `finalize_upload`
                // flipped it `pending -> available` between the list query
                // and this delete. Either way there is nothing left to
                // reclaim: no blob delete, no orphan-file check, no debit.
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

    /// After deleting a file's last abandoned pending version, check whether
    /// the parent `files` row is now a permanent zero-version orphan (no
    /// versions left **and** `content_id IS NULL`) and delete it too if so.
    ///
    /// The checks here are a cheap pre-filter run against a fresh (but
    /// pre-transaction) snapshot -- to skip the extra round-trip on the
    /// common case where the file still has other versions or content. The
    /// authoritative guard re-runs the same two checks fresh **inside** the
    /// same transaction as the file delete
    /// ([`crate::domain::ports::CleanupStore::delete_orphan_file_with_event`]),
    /// so a version inserted or bound in the gap between this pre-check and
    /// that call cannot cause data loss: the delete simply aborts and the
    /// file (with its new version) is left untouched.
    ///
    /// Returns `1` if the file row was deleted, `0` otherwise.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
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
                // @cpt-cf-file-storage-fr-usage-reporting
                // The file itself was credited `+1` at `create_file` time and
                // never got any bytes credited (its only version(s) were
                // reclaimed as abandoned pending, never finalized) -- debit
                // the file count only; `bytes_delta` is `0` because this is,
                // by construction, a zero-version file (see
                // `orphan_candidate_file`).
                self.report_usage(UsageDelta {
                    tenant_id: file.tenant_id,
                    owner_id: file.owner_id,
                    bytes_delta: 0,
                    file_count_delta: -1,
                });
                1
            }
            Ok(false) => {
                // Guard failed inside the transaction (a version now exists
                // / is bound) or a concurrent sweep already removed it --
                // both fine.
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

    /// Pre-check (fresh, but pre-transaction) whether `file_id` looks like a
    /// permanent zero-version orphan: no remaining versions and a `NULL`
    /// `content_id`. Returns the `File` row to delete if so, `None` if it is
    /// not (or no longer) an orphan, or a lookup failed (logged).
    ///
    /// Extracted from [`Self::maybe_delete_orphaned_file`] to keep its
    /// cognitive complexity down; see that method's docs for why this being
    /// a pre-transaction snapshot is safe.
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
            Ok(None) => return None, // Already gone -- fine.
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
            // Bound content means a version exists (the `remaining` snapshot
            // above must be stale) -- leave the file alone.
            return None;
        }

        if self.has_blocking_multipart_session(file_id).await {
            return None;
        }

        Some(file)
    }

    /// Whether `file_id` has a not-yet-expired multipart session that should
    /// block orphan-file deletion (P2 2.8).
    ///
    /// `sweep_abandoned_pending` keys only on a pending version's age, so a
    /// multipart session that has legitimately not expired yet can still have
    /// its backing version aged past the orphan grace window and reclaimed
    /// earlier in the same sweep pass. If [`Self::orphan_candidate_file`]'s
    /// caller went on to delete the file here too, the `files` FK's
    /// `ON DELETE CASCADE` would take the still-`in_progress`
    /// `multipart_uploads` row with it, destroying a live upload with no
    /// error surfaced to the caller. Returning `true` leaves the file for a
    /// later sweep instead -- once the session is aborted/completed (by
    /// `sweep_expired_multipart` or the user), a subsequent pass will find
    /// zero versions and no in-progress session, and finish reclaiming it
    /// then. A lookup failure is treated as blocking (logged), erring toward
    /// not deleting.
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
    // @cpt-begin:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-list
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
        // @cpt-end:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-list

        let mut count = 0_usize;
        for session in sessions {
            count += self.abort_expired_multipart_session(session).await;
        }
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-return
        count
        // @cpt-end:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-return
    }

    /// Abort one expired multipart session: win the session's own
    /// `in_progress -> aborted` CAS *first*, and only on success clean up the
    /// backend upload handle and delete the pending version row.
    ///
    /// The CAS must run before version cleanup, not after: this is exactly
    /// the CAS-first pattern the user-driven `abort_multipart_upload` path
    /// already uses. A concurrent `complete_multipart_upload` races against
    /// this same session-row CAS (`in_progress -> completed` vs.
    /// `in_progress -> aborted`) -- only one of them can win. If the sweep
    /// loses (`Ok(false)`), a concurrent complete may have already bound this
    /// version, so it must be left completely untouched.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    /// @cpt-state:cpt-cf-file-storage-state-retention-cleanup-multipart-touch:p1
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
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-cas
        match self
            .store
            .abort_multipart_upload(session.upload_id, abort_audit)
            .await
        {
            // @cpt-end:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-cas
            // @cpt-begin:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-cleanup
            Ok(true) => {
                // We won the CAS: no concurrent complete can have bound this
                // version afterward. Safe to clean up the backend handle and
                // delete the pending version row.
                self.cleanup_expired_session_version(&session).await;
                1
            }
            // @cpt-end:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-cleanup
            // @cpt-begin:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-skip
            Ok(false) => {
                // A concurrent complete/abort already transitioned the
                // session out of in_progress. If it was `complete`, the
                // version is now Available and bound -- do NOT touch it.
                tracing::info!(
                    upload_id = %session.upload_id,
                    "cleanup: skipping version cleanup, session no longer in_progress \
                     (concurrent complete/abort won the race)"
                );
                0
            }
            // @cpt-end:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-skip
            Err(e) => {
                tracing::warn!(error = ?e, upload_id = %session.upload_id,
                    "cleanup: failed to mark expired multipart upload as aborted");
                0
            }
        }
    }

    /// Helper: abort the backend upload and delete the pending version row for
    /// an expired multipart session. Both operations are best-effort.
    ///
    /// `pub` (rather than private) solely so the P2 0.3 step-5 unit test can
    /// invoke it directly to exercise the narrow mid-flight interleaving
    /// window deterministically, without real concurrency: this function is
    /// otherwise only ever called from `abort_expired_multipart_session`
    /// after that method has already won the session CAS.
    ///
    /// The version row backing this session may already be gone by the time
    /// this runs: step 1 of the same sweep (`sweep_abandoned_pending`) only
    /// excludes sessions with `expires_at > now` (still live), so an
    /// *expired-but-still-`in_progress`* session's pending version can be
    /// reclaimed by step 1 before step 2 (this method) ever sees it. When
    /// that happens the backend multipart upload handle must still be
    /// aborted -- otherwise it leaks (e.g. an incomplete S3 multipart upload
    /// and its uploaded parts) -- so the backend abort is attempted
    /// regardless of whether the version row is still present, falling back
    /// to the default backend and the deterministic `(file_id, version_id)`
    /// path when it is not (mirrors `MultipartService::abort_multipart_upload`'s
    /// own `version.is_none()` fallback for the same reason).
    pub async fn cleanup_expired_session_version(&self, session: &MultipartUploadSession) {
        let ver = self
            .store
            .get_version(session.file_id, session.version_id)
            .await
            .ok()
            .flatten();

        // Best-effort: tell the backend to discard the in-progress upload.
        // Resolve `(backend_id, backend_path)` from the version row when it
        // is still there; otherwise fall back to the default backend and the
        // deterministic path -- see the doc comment above for why this must
        // not be skipped just because the version row is already reclaimed.
        let (backend_id, backend_path) = ver.as_ref().map_or_else(
            || {
                (
                    self.backends.default_id().to_owned(),
                    expired_session_backend_path(session.file_id, session.version_id),
                )
            },
            |v| (v.backend_id.clone(), v.backend_path.clone()),
        );
        self.backend_abort_multipart_best_effort(
            &backend_id,
            &backend_path,
            &session.backend_upload_handle,
            session.upload_id,
        )
        .await;

        // Best-effort: delete the pending version row (a no-op, matching
        // zero rows, when step 1 already reclaimed it above). Status-guarded
        // (P2 0.3 step 5): only deletes if the row is still `pending`, so a
        // version that a racing `complete_multipart_upload` already flipped
        // to `available` (via `finalize_version`, ahead of its own session
        // CAS) is left untouched -- the DELETE simply matches zero rows.
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

    /// Delete files that have been expired by a retention rule.
    ///
    /// Files are scanned in keyset-paginated batches (by `file_id`) so the sweep
    /// never materializes every file across every tenant at once — memory stays
    /// bounded regardless of deployment size. Retention rules are fetched once
    /// and reused across batches (the rule set is small relative to the files).
    // @cpt-begin:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-rules
    async fn sweep_retention_expiry(&self, now: OffsetDateTime) -> usize {
        let all_rules = match self.store.list_all_retention_rules().await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = ?e, "cleanup: failed to list retention rules");
                return 0;
            }
        };
        // No rules configured → nothing to expire; skip the file scan entirely.
        if all_rules.is_empty() {
            return 0;
        }
        // @cpt-end:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-rules

        // @cpt-begin:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-scan
        let mut count = 0_usize;
        let mut after: Option<Uuid> = None;
        // Keyset cursor loop: each page advances `after` past its last file_id.
        // Safe even though `expire_batch` deletes rows — the next query filters
        // `file_id > after`, so deletions never shift the window. A short page
        // (or `None` from a query error) ends the sweep.
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
        // @cpt-end:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-scan
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-return
        count
        // @cpt-end:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-return
    }

    /// Fetch the next keyset page of files for the retention sweep. Returns
    /// `None` (ending the sweep) on a query error, logging it best-effort.
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

    /// Check and apply retention rules to one file. Returns 1 if deleted, 0 otherwise.
    async fn maybe_expire_file(
        &self,
        file: &file_storage_sdk::File,
        all_rules: &[crate::domain::policy::StoredRetentionRule],
        now: OffsetDateTime,
    ) -> usize {
        // Gather applicable rules: tenant-scope, user-scope (owner), file-scope.
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-applicable
        let applicable: Vec<&crate::domain::policy::StoredRetentionRule> = all_rules
            .iter()
            .filter(|r| rule_applies_to_file(r, file))
            .collect();

        if applicable.is_empty() {
            return 0;
        }
        // @cpt-end:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-applicable

        // Fetch custom metadata for metadata-criterion rules.
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-metadata
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
        // @cpt-end:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-metadata

        // OR semantics: if any rule triggers, delete the file.
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-match
        let should_expire = applicable
            .iter()
            .any(|r| rule_matches(&r.body, file, &metadata, now));

        if !should_expire {
            return 0;
        }
        // @cpt-end:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-match

        // @cpt-begin:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-delete
        self.expire_file(file, now).await
        // @cpt-end:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-delete
    }

    /// Fetch a file's versions ahead of a retention deletion. Returns `None`
    /// (after logging) if the store errors, so the caller can skip expiring
    /// this file rather than treating the error as "zero versions" and
    /// deleting it anyway.
    ///
    /// Extracted from `expire_file` to keep its cognitive complexity down.
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
        // Collect version blobs before deleting so we can clean them up
        // after the DB row is gone.
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
                // @cpt-cf-file-storage-fr-usage-reporting
                // Debit the file's total bytes and the file count -- a
                // retention-expired delete removes the whole file (mirrors
                // `FileService::delete_file_inner`'s debit for the
                // user-initiated path).
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

/// Deterministic backend path for a `(file_id, version_id)` pair.
///
/// Mirrors `FileService::backend_path`/`MultipartService::backend_path` (both
/// `format!("/{file_id}/{version_id}")`) -- duplicated here rather than
/// reached into from another domain service, since it is a pure, stateless
/// computation with no dependency on either service. Used only as a fallback
/// by [`CleanupEngine::cleanup_expired_session_version`] when the version row
/// backing an expired multipart session is already gone (reclaimed by step 1
/// of the same sweep) and so cannot supply its own `backend_path` column.
fn expired_session_backend_path(file_id: Uuid, version_id: Uuid) -> String {
    format!("/{file_id}/{version_id}")
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
