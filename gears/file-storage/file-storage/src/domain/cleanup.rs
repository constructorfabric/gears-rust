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

        // Step 2 -- expired multipart sessions. FS-05/F10 fix: this can now
        // ALSO reclaim a zero-version orphan file left behind by step 1
        // above (step 1's own orphan check runs while this session still
        // looks in_progress, and correctly declines then).
        // @cpt-begin:cpt-cf-file-storage-algo-run-sweep:p1:inst-sweep-step2
        let (expired_aborted, files_deleted_by_step2) = self.sweep_expired_multipart(now).await;
        result.expired_multipart_aborted += expired_aborted;
        result.abandoned_files_deleted += files_deleted_by_step2;
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
                // Reuse the `file` snapshot already read (via
                // `load_file_for_audit`) above for this function's own audit
                // row, instead of letting `orphan_candidate_file` fetch it a
                // second time -- same read-elimination as
                // `cleanup_expired_session_version_with_file`'s.
                let files_deleted = self.maybe_delete_orphaned_file(file_id, file).await;
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
    /// `prefetched_file` lets a caller that has already read this `File` row
    /// moments ago (for its own audit-row `tenant_id`, typically) hand it down
    /// so [`Self::orphan_candidate_file`] does not read it a third time for
    /// the same file -- pass `None` when no such snapshot is available, and
    /// this fetches fresh exactly as it always did.
    ///
    /// @cpt-cf-file-storage-fr-orphan-reconciliation
    async fn maybe_delete_orphaned_file(
        &self,
        file_id: Uuid,
        prefetched_file: Option<file_storage_sdk::File>,
    ) -> usize {
        let Some(file) = self.orphan_candidate_file(file_id, prefetched_file).await else {
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
    ///
    /// `remaining` (the version list) is always re-fetched fresh here
    /// regardless of `prefetched_file` -- that check is the entire point of
    /// this pre-check and a caller-supplied `File` snapshot says nothing
    /// about it. `prefetched_file`, when `Some`, is used in place of this
    /// method's own `get_file` call for the `content_id`/`tenant_id`/
    /// `owner_id` fields only; it may be a little older than a fresh read
    /// would be (taken before whatever version-row cleanup the caller just
    /// did), but per this method's own doc comment above, that staleness
    /// cannot cause an incorrect delete -- only, in a rare race, one extra
    /// delete attempt that the transactional guard safely turns into a
    /// no-op. `None` reproduces the old always-fresh `get_file` call exactly,
    /// including its "already gone" (`Ok(None)`) and error handling.
    async fn orphan_candidate_file(
        &self,
        file_id: Uuid,
        prefetched_file: Option<file_storage_sdk::File>,
    ) -> Option<file_storage_sdk::File> {
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

        let file = self
            .resolve_orphan_candidate_row(file_id, prefetched_file)
            .await?;
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

    /// Resolve the `files` row the orphan check needs: the caller's own
    /// snapshot when it already read one moments ago, otherwise a fresh
    /// lookup. `None` means "do not treat this file as an orphan" -- either
    /// the row is already gone (nothing left to reclaim) or the lookup
    /// failed, in which case erring toward not deleting is the safe
    /// direction. Split out of [`Self::orphan_candidate_file`] purely to
    /// keep that method under the crate's cognitive-complexity ceiling.
    async fn resolve_orphan_candidate_row(
        &self,
        file_id: Uuid,
        prefetched_file: Option<file_storage_sdk::File>,
    ) -> Option<file_storage_sdk::File> {
        if let Some(file) = prefetched_file {
            return Some(file);
        }
        match self.store.get_file(file_id).await {
            Ok(Some(file)) => Some(file),
            Ok(None) => None, // Already gone -- fine.
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    "cleanup: failed to fetch file while checking for orphaned file"
                );
                None
            }
        }
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
    /// Returns `(sessions_aborted, orphan_files_reclaimed)` -- the second
    /// tally is FS-05/F10's fix: an expired session's own cleanup can now
    /// also reclaim a zero-version parent file, not just step 1's.
    // @cpt-begin:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-list
    async fn sweep_expired_multipart(&self, now: OffsetDateTime) -> (usize, usize) {
        let sessions = match self.store.list_expired_multipart_uploads(now).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "cleanup: failed to list expired multipart uploads"
                );
                return (0, 0);
            }
        };
        // @cpt-end:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-list

        let mut aborted_count = 0_usize;
        let mut files_count = 0_usize;
        for session in sessions {
            let (aborted, files) = self.abort_expired_multipart_session(session).await;
            aborted_count += aborted;
            files_count += files;
        }
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-return
        (aborted_count, files_count)
        // @cpt-end:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-return
    }

    /// Abort one expired multipart session: win the session's own
    /// `in_progress -> aborted` CAS *first*, and only on success clean up the
    /// backend upload handle and delete the pending version row. Returns
    /// `(sessions_aborted, orphan_files_reclaimed)`, each `0` or `1`.
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
    async fn abort_expired_multipart_session(
        &self,
        session: MultipartUploadSession,
    ) -> (usize, usize) {
        // Read the parent `File` once, here, and thread it all the way down
        // through `cleanup_expired_session_version_with_file` to
        // `orphan_candidate_file`, instead of letting each of those three
        // spots fetch it independently (three `get_file` calls -- one per
        // spot -- collapsing to this one for every expired session the sweep
        // processes). `.ok().flatten()` deliberately keeps the original
        // silent-on-error fallback (`Uuid::nil()` for the audit tenant when
        // the lookup fails) unchanged; see the "known open issue" note on
        // that fallback elsewhere in this module -- this fix is about read
        // count, not about that fallback's behavior.
        let file = self.store.get_file(session.file_id).await.ok().flatten();
        let audit_tenant_id = file.as_ref().map_or_else(Uuid::nil, |file| file.tenant_id);
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
                //
                // Calls the `_with_file` variant directly (not the public
                // `cleanup_expired_session_version` wrapper) so the `file`
                // read above is reused instead of re-fetched -- see that
                // variant's doc comment.
                let files_reclaimed = self
                    .cleanup_expired_session_version_with_file(&session, file)
                    .await;
                (1, files_reclaimed)
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
                (0, 0)
            }
            // @cpt-end:cpt-cf-file-storage-algo-sweep-expired-multipart:p1:inst-sweep-multipart-skip
            Err(e) => {
                tracing::warn!(error = ?e, upload_id = %session.upload_id,
                    "cleanup: failed to mark expired multipart upload as aborted");
                (0, 0)
            }
        }
    }

    /// Helper: abort the backend upload and delete the pending version row for
    /// an expired multipart session, then (FS-05/F10 fix) check whether that
    /// leaves the parent file a permanent zero-version orphan. Returns `1` if
    /// the orphan file was also reclaimed here, `0` otherwise.
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
    ///
    /// FS-05/F10 fix: this method now finishes with the SAME
    /// `maybe_delete_orphaned_file` check `delete_abandoned_pending_version`
    /// already runs after its own version delete. Before this fix, the
    /// orphan-file check only ever ran from step 1's side -- if step 1
    /// reclaimed this exact version first (this session's `expires_at` had
    /// passed, but step 2 hadn't aborted it yet), step 1's own
    /// `has_in_progress_for_file` check still saw the session as
    /// `in_progress` (step 2 runs after step 1 within one `run_sweep` pass)
    /// and correctly declined to delete the parent file at that moment --
    /// but nothing ever re-checked afterward, since the orphan check only
    /// ever ran as an immediate side effect of a version delete, and there
    /// was no version left to trigger it a second time. The parent file was
    /// left a permanent, never-revisited version-less orphan ("a second path
    /// into F1", `upload-flow-review.md`'s F10). Now: whichever of the two cleanup
    /// paths (step 1's version reclaim, or step 2's own version cleanup
    /// here) runs last for a given file also gets a fresh chance to notice
    /// the file has zero versions and a `NULL` `content_id` -- by the time
    /// THIS method runs, `abort_expired_multipart_session` has already won
    /// the session's own CAS to `aborted`, so `has_in_progress_for_file` no
    /// longer sees it as blocking, and the orphan is correctly reclaimed
    /// within the same sweep pass, symmetric with step 1's own path.
    ///
    /// This is a thin wrapper over
    /// [`Self::cleanup_expired_session_version_with_file`] with no prefetched
    /// `File` (`None`), kept `pub` with this exact signature specifically
    /// because the unit test named above calls it directly; changing it
    /// would break that test, which this crate's other agents/files may
    /// depend on. The real sweep path (`abort_expired_multipart_session`)
    /// calls the `_with_file` variant instead, passing the `File` it already
    /// read for its own audit row -- see that variant's doc comment for why.
    pub async fn cleanup_expired_session_version(&self, session: &MultipartUploadSession) -> usize {
        self.cleanup_expired_session_version_with_file(session, None)
            .await
    }

    /// Implementation behind [`Self::cleanup_expired_session_version`],
    /// parameterized on an optional already-read parent `File`.
    ///
    /// Two spots in this method need the parent `File` (or at least its
    /// `tenant_id`): the pending-version-delete audit row below, and --
    /// transitively, via [`Self::maybe_delete_orphaned_file`] ->
    /// [`Self::orphan_candidate_file`] -- the zero-version orphan pre-check
    /// at the end. Previously each fetched it independently with its own
    /// `get_file` call, and `abort_expired_multipart_session` (the only
    /// production caller) had *already* read the same file a third time just
    /// before calling in, for its own "session aborted" audit row -- three
    /// reads of one row per expired session. `prefetched_file` lets a caller
    /// that already has a (possibly slightly stale) snapshot hand it down
    /// instead: reused here for the delete-audit `tenant_id`, and passed
    /// through unchanged to `maybe_delete_orphaned_file` so
    /// `orphan_candidate_file` can skip its own `get_file` too.
    ///
    /// A `None` (the public wrapper's case, e.g. the unit test invoking it
    /// standalone) reproduces the old fully-fresh, three-reads-worth-of-work
    /// behavior exactly -- this only removes *redundant* reads, it does not
    /// change what gets read when there is nothing to reuse.
    ///
    /// Reusing a snapshot taken slightly earlier (before the backend abort
    /// and the pending-version delete this method performs) is safe for both
    /// uses: `tenant_id` does not change as a side effect of aborting an
    /// upload, and the orphan pre-check's own doc comment already establishes
    /// that staleness here cannot cause data loss -- `delete_orphan_file_with_event`
    /// re-verifies the same zero-versions/`NULL`-`content_id` condition fresh
    /// *inside* its own transaction, so a pre-check working off a slightly
    /// older snapshot only risks one extra, safely-aborted delete attempt in
    /// an already-rare race window, never an incorrect deletion.
    async fn cleanup_expired_session_version_with_file(
        &self,
        session: &MultipartUploadSession,
        prefetched_file: Option<file_storage_sdk::File>,
    ) -> usize {
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
        // Reuse `prefetched_file` when the caller already has it; otherwise
        // fetch it here, exactly as this used to unconditionally do. Either
        // way this is now the ONE read of the file this method performs (the
        // same snapshot, if present, is handed to `maybe_delete_orphaned_file`
        // below instead of triggering yet another read there).
        let file = match prefetched_file {
            Some(file) => Some(file),
            None => self.store.get_file(session.file_id).await.ok().flatten(),
        };
        let del_audit = orphan_reconcile_audit(
            session.file_id,
            file.as_ref().map_or_else(Uuid::nil, |file| file.tenant_id),
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

        // FS-05/F10 fix: this session is now `aborted` (the caller only
        // reaches this method after winning that CAS), so
        // `has_in_progress_for_file` no longer blocks reclaiming a
        // zero-version, NULL-content_id parent -- whether the version was
        // just deleted above, or already reclaimed earlier by step 1's own
        // path (`delete_abandoned_pending_version`, whose own attempt was
        // correctly blocked while this session still looked in-progress).
        //
        // `file` (the same snapshot used for `del_audit`'s tenant_id above)
        // is handed down so `orphan_candidate_file` does not re-fetch it --
        // see `cleanup_expired_session_version_with_file`'s doc comment for
        // why that reuse is safe.
        self.maybe_delete_orphaned_file(session.file_id, file).await
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
        // N+1 fix (metadata half, page level): fetch the custom metadata of
        // every file on this page that actually needs it in ONE query,
        // instead of one query per file inside `maybe_expire_file`. Files
        // whose applicable rules carry no metadata criterion are not fetched
        // at all (see `needs_metadata`), so a deployment with only
        // age/inactivity rules -- the common case -- issues zero metadata
        // queries per page, and one with metadata rules issues exactly one.
        //
        // `RETENTION_SWEEP_BATCH` is 500, so the `IN (...)` list binds at
        // most 500 parameters, an order of magnitude below the smallest
        // backend budget `max_bind_params_for` reports (30_000 on SQLite) --
        // no chunking is needed here, unlike `MetadataRepo::delete_keys`
        // whose list length follows an unbounded client request.
        let need_metadata: Vec<Uuid> = batch
            .iter()
            .filter(|f| Self::needs_metadata(all_rules, f))
            .map(|f| f.file_id)
            .collect();
        let prefetched = if need_metadata.is_empty() {
            Some(std::collections::HashMap::new())
        } else {
            match self.store.list_metadata_for_files(&need_metadata).await {
                Ok(map) => Some(map),
                Err(e) => {
                    // Mirrors the old per-file failure behaviour, page-wide:
                    // a file whose rules need metadata we could not read is
                    // skipped (never expired on incomplete information),
                    // while files whose rules need no metadata are still
                    // evaluated normally below.
                    tracing::warn!(
                        error = ?e,
                        page_size = batch.len(),
                        "cleanup: failed to batch-fetch metadata for retention check -- \
                         files with metadata-criterion rules are skipped this pass"
                    );
                    None
                }
            }
        };

        let mut count = 0_usize;
        for file in batch {
            count += self
                .maybe_expire_file(file, all_rules, now, prefetched.as_ref())
                .await;
        }
        count
    }

    /// Whether any retention rule applicable to `file` carries a metadata
    /// criterion -- i.e. whether [`Self::maybe_expire_file`] will need this
    /// file's custom metadata to reach a verdict.
    ///
    /// Shared by [`Self::expire_batch`]'s page-level prefetch and
    /// `maybe_expire_file`'s own evaluation so the two can never disagree
    /// about which files were fetched: if this says `false`, no query is
    /// issued and `rule_matches` is handed an empty slice, which it treats
    /// identically (it only reads `metadata` inside its
    /// `body.metadata.is_some()` branch).
    fn needs_metadata(
        all_rules: &[crate::domain::policy::StoredRetentionRule],
        file: &file_storage_sdk::File,
    ) -> bool {
        all_rules
            .iter()
            .any(|r| rule_applies_to_file(r, file) && r.body.metadata.is_some())
    }

    /// Check and apply retention rules to one file. Returns 1 if deleted, 0 otherwise.
    ///
    /// N+1 fix (metadata half): this used to call
    /// [`CleanupStore::list_metadata`] unconditionally for every file with at
    /// least one applicable rule -- one query per such file, every
    /// `sweep_interval_secs`, for the lifetime of the deployment -- even
    /// though [`rule_matches`] only ever consults `metadata` inside its
    /// `body.metadata.is_some()` branch. A deployment with a single
    /// tenant-scope age/inactivity rule (the common case: one rule, applying
    /// to every file) paid for a metadata query per file for data no rule
    /// ever looked at. Skipping the fetch below when no applicable rule has
    /// a metadata criterion is exactly as correct as fetching real metadata
    /// and passing it in, since `rule_matches` would ignore it either way --
    /// it is not a behavior change, only fewer queries.
    ///
    /// The page-level half is fixed too: [`Self::expire_batch`] now
    /// prefetches the metadata of every file on the page that needs it in a
    /// single [`CleanupStore::list_metadata_for_files`] query (the same
    /// batching `GET /files` uses on the read path) and hands the result in
    /// via `prefetched_metadata`, so this method issues no queries of its
    /// own at all. `None` there means that batch fetch failed, in which case
    /// a file whose rules need metadata is skipped rather than expired on
    /// data that could not be read.
    ///
    async fn maybe_expire_file(
        &self,
        file: &file_storage_sdk::File,
        all_rules: &[crate::domain::policy::StoredRetentionRule],
        now: OffsetDateTime,
        prefetched_metadata: Option<
            &std::collections::HashMap<Uuid, Vec<file_storage_sdk::CustomMetadataEntry>>,
        >,
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

        // Fetch custom metadata only when some applicable rule actually has a
        // metadata criterion -- see this method's doc comment. `rule_matches`
        // treats an empty slice identically to "no matching metadata entry",
        // and it never reaches the `metadata` parameter at all for a rule
        // whose `body.metadata` is `None`, so this is semantically a no-op
        // for every rule that doesn't need it.
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-metadata
        let metadata: &[file_storage_sdk::CustomMetadataEntry] =
            if applicable.iter().any(|r| r.body.metadata.is_some()) {
                match prefetched_metadata {
                    // Absent from the map == this file simply has no custom
                    // metadata rows, which `rule_matches` treats the same as
                    // "no entry matched".
                    Some(map) => map.get(&file.file_id).map_or(&[][..], Vec::as_slice),
                    // The page-level batch fetch failed (already logged by
                    // `expire_batch`): skip rather than expire a file on
                    // metadata we could not read.
                    None => return 0,
                }
            } else {
                &[]
            };
        // @cpt-end:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-metadata

        // OR semantics: if any rule triggers, delete the file.
        // @cpt-begin:cpt-cf-file-storage-algo-sweep-retention-expiry:p1:inst-sweep-retention-match
        let should_expire = applicable
            .iter()
            .any(|r| rule_matches(&r.body, file, metadata, now));

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
