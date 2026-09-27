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

#![allow(unknown_lints, de0309_must_have_domain_model)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use time::OffsetDateTime;
use uuid::Uuid;

use crate::domain::audit::{AuditEntry, AuditOperation, AuditOutcome, FileEvent};
use crate::domain::multipart::MultipartUploadSession;
use crate::domain::policy::RetentionScope;
use crate::domain::ports::{CleanupStore, DeletedFile};
use crate::domain::storage_layout;
use crate::infra::backend::BackendRegistry;
use crate::infra::external_clients::{UsageDelta, UsageReporter};

/// Page size for the keyset-paginated retention file scan. Bounds how many
/// `File` rows the sweep holds in memory at once, independent of total count.
const RETENTION_SWEEP_BATCH: u64 = 500;

/// Page size for [`CleanupEngine::sweep_abandoned_pending_page`] (sweep step
/// 1, first phase): abandoned pending version rows reclaimed per batch. A
/// tick keeps calling this phase, keyset-paginated by `(created_at,
/// version_id)`, batch after batch (interleaved with the other sweep phases)
/// until it comes up short or [`CleanupEngine`]'s per-tick time budget runs
/// out; whatever is left is picked up either later in the same tick or, once
/// the budget is spent, by the next scheduled tick.
const ABANDONED_PENDING_SWEEP_BATCH: u64 = 500;

/// Page size for the second phase of sweep step 1
/// ([`CleanupEngine::sweep_versionless_files_page`]): permanently versionless
/// `files` rows reclaimed per batch. Same keyset-pagination and time-budget
/// reasoning as [`ABANDONED_PENDING_SWEEP_BATCH`] above, paginated by
/// `(created_at, file_id)`.
const VERSIONLESS_SWEEP_BATCH: u64 = 500;

/// Page size for [`CleanupEngine::sweep_expired_multipart_page`] (sweep step
/// 2): expired multipart sessions aborted per batch. Same keyset-pagination
/// and time-budget reasoning as [`ABANDONED_PENDING_SWEEP_BATCH`] above,
/// paginated by `(expires_at, upload_id)`.
const EXPIRED_MULTIPART_SWEEP_BATCH: u64 = 500;

/// Page size for sweep step 4 (expired `idempotency_keys` rows) per batch.
/// No cursor is needed here: each batch deletes the rows it selects, so the
/// next call (later in the same tick, or the next tick) naturally sees the
/// next-oldest backlog rather than the same rows again. A stalled sweep
/// letting a large backlog accumulate must not turn one tick into a single
/// unbounded `DELETE` (a long-held lock on `PostgreSQL`, one large
/// single-writer transaction on `SQLite`).
const EXPIRED_IDEMPOTENCY_SWEEP_BATCH: u64 = 500;

/// Default per-tick time budget for [`CleanupEngine::run_sweep`] -- used
/// whenever [`CleanupEngine::with_tick_budget`] is never called (e.g. every
/// test in this crate that builds a `CleanupEngine` directly, and the
/// `CleanupEngine::new(...)` call in `gear.rs` before `.with_tick_budget(..)`
/// is applied). Mirrors `FileStorageConfig::sweep_time_budget_secs`'s own
/// default (900s / 15 minutes) -- see that field's doc comment for why.
const DEFAULT_SWEEP_TICK_BUDGET: Duration = Duration::from_mins(15);

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
    /// Number of permanent zero-version, `NULL`-`content_id` orphan `files`
    /// rows deleted -- whether reclaimed as a side effect of step 1's
    /// abandoned-pending-version phase or step 2's expired-multipart-session
    /// phase (a `file_versions` row existed and was aged out, leaving the
    /// parent with zero versions), or by step 1's dedicated
    /// versionless-files phase ([`CleanupEngine::sweep_versionless_files_page`])
    /// for a `files` row that never received a version at all.
    pub abandoned_files_deleted: usize,
    /// Number of expired in-progress multipart sessions aborted.
    pub expired_multipart_aborted: usize,
    /// Number of files deleted because a retention rule triggered.
    pub retention_expired_deleted: usize,
    /// Number of expired `idempotency_keys` rows deleted.
    pub idempotency_keys_deleted: u64,
    /// `true` when this tick stopped because its time budget
    /// ([`CleanupEngine::with_tick_budget`] / `sweep_time_budget_secs`) ran
    /// out while at least one sweep phase still had more candidates to
    /// process, `false` when every phase ran to exhaustion on its own. A
    /// budget-exhausted tick is not an error -- the unfinished phases simply
    /// resume (pending/versionless/multipart from the start of their keyset
    /// order again; retention from its saved cursor) on a later tick.
    pub budget_exhausted: bool,
    /// Wall-clock duration of this sweep tick, in milliseconds.
    pub elapsed_ms: u64,
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
pub struct CleanupEngine {
    store: Arc<dyn CleanupStore>,
    backends: BackendRegistry,
    config: CleanupConfig,
    /// Usage-reporting sink. `None` disables reporting (fire-and-forget
    /// no-op); `gear.rs` opts in via [`Self::with_usage_reporter`] once a
    /// Usage Collector client is wired.
    usage_reporter: Option<Arc<dyn UsageReporter>>,
    /// Per-tick time budget for [`Self::run_sweep`]. Defaults to
    /// [`DEFAULT_SWEEP_TICK_BUDGET`]; `gear.rs` overrides it via
    /// [`Self::with_tick_budget`] from `FileStorageConfig::sweep_time_budget_secs`.
    tick_budget: Duration,
    /// Retention-sweep keyset cursor (`files.file_id`), carried **across**
    /// ticks -- unlike the other sweep phases' cursors, which
    /// [`Self::run_sweep`] keeps as tick-local variables and always restarts
    /// at `None`. `None` means "start from the beginning" (either nothing
    /// was in progress, or the last tick finished the whole table). See
    /// [`Self::sweep_retention_expiry_page`] for how it advances.
    retention_cursor: Mutex<Option<Uuid>>,
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
            tick_budget: DEFAULT_SWEEP_TICK_BUDGET,
            retention_cursor: Mutex::new(None),
        }
    }

    /// Install a usage-reporting sink. Kept as a builder step (mirroring
    /// `FileService`/`MultipartService`'s `with_metrics`/`with_usage_reporter`)
    /// so existing `CleanupEngine::new(...)` call sites keep compiling unchanged.
    #[must_use]
    pub fn with_usage_reporter(mut self, usage_reporter: Option<Arc<dyn UsageReporter>>) -> Self {
        self.usage_reporter = usage_reporter;
        self
    }

    /// Override the per-tick time budget (default: [`DEFAULT_SWEEP_TICK_BUDGET`]).
    /// Builder step, same shape as [`Self::with_usage_reporter`]; `gear.rs`
    /// calls this once at init with `FileStorageConfig::sweep_time_budget_secs`.
    #[must_use]
    pub fn with_tick_budget(mut self, tick_budget: Duration) -> Self {
        self.tick_budget = tick_budget;
        self
    }

    /// Fire-and-forget usage delta report. Failures are logged but never
    /// propagated -- a failing usage reporter must not block the sweep.
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
    ///    the orphan grace window) -- **except** a version still backing an
    ///    active multipart session: a live `in_progress` one (`expires_at >
    ///    now`), or any `completing` one (regardless of lease), neither of
    ///    which is ever selected regardless of age. Followed by a second phase,
    ///    [`Self::sweep_versionless_files_page`], for `files` rows that never
    ///    got a version row in the first place (so the first phase's own
    ///    version-age query can never see them) -- e.g. a process crash
    ///    between `FileService::create_file_bare`'s commit and
    ///    `MultipartService::initiate_multipart_upload`'s
    ///    `insert_pending_version`, or a failed
    ///    `FileService::compensate_failed_multipart_initiate`.
    /// 2. Expired multipart sessions (`expires_at < now`, still `in_progress`,
    ///    or `completing` with a lapsed lease).
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
    /// # Time budget and multi-pass batching
    ///
    /// A single call to any one phase only ever processes one bounded batch
    /// (`ABANDONED_PENDING_SWEEP_BATCH` and siblings, `500` rows). Rather than
    /// stopping there, this method repeats the four steps above as further
    /// **passes**, in the same order, each pass fetching one more batch per
    /// not-yet-exhausted phase, until either every phase is exhausted (its
    /// last batch came back short, or its query/delete errored) or the
    /// per-tick time budget ([`Self::with_tick_budget`], default
    /// [`DEFAULT_SWEEP_TICK_BUDGET`]) runs out. The very first pass always
    /// runs in full regardless of the budget -- even a zero budget still
    /// processes one batch per phase -- so a misconfigured budget can never
    /// make a tick do *less* than the old single-batch-per-call behaviour.
    /// The deadline is checked before every batch from the second pass
    /// onward; `now`/`grace_cutoff` are sampled once at the top and reused
    /// for every pass and every phase, so a guard that depends on "now" (see
    /// `sweep_abandoned_pending_page`'s doc) sees one consistent instant for
    /// the whole tick.
    ///
    /// Phases 1/1b/2 each keep a keyset cursor **local to this call**,
    /// starting at `None` (oldest first) every tick regardless of how far a
    /// previous tick got -- a stuck head-of-line candidate (an active
    /// multipart session, a transient per-candidate error) is retried from
    /// scratch next tick, and free candidates further back in keyset order
    /// are no longer starved behind it within *this* tick. Phase 3
    /// (retention) is the exception: its cursor survives across ticks (see
    /// [`Self::sweep_retention_expiry_page`]) because a full table scan can
    /// legitimately span many ticks, and restarting it from scratch every
    /// tick would starve files later in `file_id` order on a large enough
    /// deployment. Phase 4 (idempotency) needs no cursor at all: each batch
    /// deletes the rows it selects, so the next call -- later this tick, or
    /// next tick -- naturally sees the next-oldest backlog.
    #[tracing::instrument(skip_all)]
    pub async fn run_sweep(&self) -> SweepResult {
        let tick_start = Instant::now();
        // `None` here means "budget large enough that `Instant + Duration`
        // would overflow" -- treated as unbounded (never times out) rather
        // than panicking or silently truncating.
        let deadline = tick_start.checked_add(self.tick_budget);

        let mut result = SweepResult::default();
        let now = OffsetDateTime::now_utc();
        let grace =
            time::Duration::seconds(i64::try_from(self.config.orphan_grace_secs).unwrap_or(3600));
        let grace_cutoff = now - grace;

        // Per-phase tick-local state: a keyset cursor (reset to `None` every
        // tick -- see doc comment above) and an "exhausted" latch that, once
        // set, skips that phase for the rest of this tick.
        let mut pending_after: Option<(OffsetDateTime, Uuid)> = None;
        let mut pending_done = false;
        let mut versionless_after: Option<(OffsetDateTime, Uuid)> = None;
        let mut versionless_done = false;
        let mut multipart_after: Option<(OffsetDateTime, Uuid)> = None;
        let mut multipart_done = false;
        let mut idempotency_done = false;

        // Retention rules are loaded once for the whole tick (unchanged from
        // the old single-pass behaviour) -- see `sweep_retention_expiry_page`.
        // An empty rule set means no scan is needed at all this tick, and the
        // persisted cursor is reset (there is nothing left to resume). A
        // failure to load the rules is deliberately NOT treated the same way:
        // it also skips the scan for this tick, but the persisted cursor from
        // a previous tick's in-progress scan must survive a transient load
        // error untouched, so that scan resumes where it left off once rule
        // loading recovers.
        let (all_rules, rules_load_failed) = match self.store.list_all_retention_rules().await {
            Ok(rules) => (rules, false),
            Err(e) => {
                tracing::warn!(error = ?e, "cleanup: failed to list retention rules");
                (Vec::new(), true)
            }
        };
        let mut retention_after = *self
            .retention_cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !rules_load_failed && all_rules.is_empty() {
            retention_after = None;
            *self
                .retention_cursor
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        }
        let mut retention_done = all_rules.is_empty();

        // Checked before each not-yet-exhausted phase's batch, but only from
        // the second pass onward -- see `run_sweep`'s doc for why the first
        // pass is unconditional.
        let deadline_hit = |first_pass: bool| -> bool {
            !first_pass
                && match deadline {
                    Some(d) => Instant::now() >= d,
                    None => false,
                }
        };

        let mut budget_exhausted = false;
        let mut first_pass = true;
        loop {
            if !pending_done {
                if deadline_hit(first_pass) {
                    budget_exhausted = true;
                    break;
                }
                let (pending_deleted, files_deleted, exhausted, next_after) = self
                    .sweep_abandoned_pending_page(grace_cutoff, now, pending_after)
                    .await;
                result.abandoned_pending_deleted += pending_deleted;
                result.abandoned_files_deleted += files_deleted;
                pending_after = next_after;
                pending_done = exhausted;
            }

            if !versionless_done {
                if deadline_hit(first_pass) {
                    budget_exhausted = true;
                    break;
                }
                let (files_deleted, exhausted, next_after) = self
                    .sweep_versionless_files_page(grace_cutoff, versionless_after)
                    .await;
                result.abandoned_files_deleted += files_deleted;
                versionless_after = next_after;
                versionless_done = exhausted;
            }

            if !multipart_done {
                if deadline_hit(first_pass) {
                    budget_exhausted = true;
                    break;
                }
                let (aborted, files_deleted, exhausted, next_after) = self
                    .sweep_expired_multipart_page(now, multipart_after)
                    .await;
                result.expired_multipart_aborted += aborted;
                result.abandoned_files_deleted += files_deleted;
                multipart_after = next_after;
                multipart_done = exhausted;
            }

            if !retention_done {
                if deadline_hit(first_pass) {
                    budget_exhausted = true;
                    break;
                }
                let (deleted, exhausted, next_after) = self
                    .sweep_retention_expiry_page(now, &all_rules, retention_after)
                    .await;
                result.retention_expired_deleted += deleted;
                retention_after = next_after;
                retention_done = exhausted;
            }

            if !idempotency_done {
                if deadline_hit(first_pass) {
                    budget_exhausted = true;
                    break;
                }
                // Step 4 -- expired idempotency-key rows. The
                // `audit_outbox`/`events_outbox` tables are deliberately NOT
                // swept here: `published_at` stays `NULL` until the Tier 4
                // EventBroker relay exists, so a row-age-based purge would
                // silently drop rows that were never delivered.
                let (deleted, exhausted) = self.sweep_idempotency_page(now).await;
                result.idempotency_keys_deleted += deleted;
                idempotency_done = exhausted;
            }

            if pending_done
                && versionless_done
                && multipart_done
                && retention_done
                && idempotency_done
            {
                break;
            }
            first_pass = false;
        }

        // Persist the retention cursor for the next tick -- see
        // `sweep_retention_expiry_page`'s doc for what each outcome means.
        *self
            .retention_cursor
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = retention_after;

        result.budget_exhausted = budget_exhausted;
        result.elapsed_ms = u64::try_from(tick_start.elapsed().as_millis()).unwrap_or(u64::MAX);
        result
    }

    // ── private sweep methods ──────────────────────────────────────────────────

    /// Delete one batch of pending version rows that were never finalised and
    /// are older than `grace_cutoff`. Blob bytes are cleaned up on a
    /// best-effort basis.
    ///
    /// Invariant: a pending version referenced by a live `in_progress`
    /// multipart session (`expires_at > now`) is never selected here,
    /// regardless of age -- see
    /// [`crate::domain::ports::CleanupStore::list_abandoned_pending_versions`].
    /// This is why `now` is threaded through alongside `grace_cutoff`: the
    /// guard must use the *same* "now" the caller used to decide the session
    /// is still live, not a value re-sampled inside the query layer.
    ///
    /// `after` is this phase's keyset cursor (`(created_at, version_id)`),
    /// `None` to start from the oldest candidate -- see [`Self::run_sweep`]'s
    /// doc for why it is tick-local rather than persisted. It exists so a
    /// candidate this batch could not reclaim (a still-blocking multipart
    /// session, a transient per-row error) does not keep re-appearing at the
    /// head of every subsequent batch **within this tick** and starve
    /// everything behind it; the cursor always advances to the last
    /// candidate this batch saw, regardless of whether that candidate was
    /// actually reclaimed.
    ///
    /// Returns `(pending_versions_deleted, orphan_files_deleted, exhausted,
    /// next_after)`. `exhausted` is `true` once this phase has nothing left
    /// to do this tick: the batch came back shorter than the page size, or
    /// the list query itself errored (logged here).
    async fn sweep_abandoned_pending_page(
        &self,
        grace_cutoff: OffsetDateTime,
        now: OffsetDateTime,
        after: Option<(OffsetDateTime, Uuid)>,
    ) -> (usize, usize, bool, Option<(OffsetDateTime, Uuid)>) {
        let versions = match self
            .store
            .list_abandoned_pending_versions(
                grace_cutoff,
                now,
                ABANDONED_PENDING_SWEEP_BATCH,
                after,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "cleanup: failed to list abandoned pending versions"
                );
                return (0, 0, true, after);
            }
        };
        let exhausted = (versions.len() as u64) < ABANDONED_PENDING_SWEEP_BATCH;
        let next_after = versions
            .last()
            .map(|v| (v.created_at, v.version_id))
            .or(after);

        // Resolve every candidate's parent file in ONE batch call, up front,
        // instead of one `get_file` per candidate inside the loop below (up
        // to `ABANDONED_PENDING_SWEEP_BATCH` round trips otherwise) -- see
        // `load_files_by_ids_for_audit`'s doc comment.
        let file_ids: Vec<Uuid> = {
            let mut seen = std::collections::HashSet::new();
            versions
                .iter()
                .map(|v| v.file_id)
                .filter(|id| seen.insert(*id))
                .collect()
        };
        let files_by_id = self.load_files_by_ids_for_audit(&file_ids).await;

        let mut pending_count = 0_usize;
        let mut files_count = 0_usize;
        for v in versions {
            let prefetched_file = files_by_id.get(&v.file_id).cloned();
            let (pending, files) = self
                .delete_abandoned_pending_version(
                    v.file_id,
                    v.version_id,
                    v.size,
                    &v.backend_id,
                    &v.backend_path,
                    prefetched_file,
                )
                .await;
            pending_count += pending;
            files_count += files;
        }
        (pending_count, files_count, exhausted, next_after)
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

    /// Batched counterpart of [`Self::load_file_for_audit`]: resolves every
    /// distinct file in `ids` in ONE round trip (`CleanupStore::list_files_by_ids`)
    /// instead of one `get_file` per candidate, keyed by `file_id` for the
    /// caller's loop to look up. Used by [`Self::sweep_abandoned_pending_page`] and
    /// [`Self::sweep_expired_multipart_page`] to resolve a whole sweep batch's
    /// (up to 500) candidates' audit `tenant_id`s up front.
    ///
    /// A failed batch read is logged once for the whole batch and treated as
    /// "no files resolved" -- every candidate in this pass falls back to a
    /// nil audit tenant, exactly as a single candidate would if its own
    /// [`Self::load_file_for_audit`] call had failed. `ids` containing no
    /// entries (an empty candidate batch) skips the call entirely.
    async fn load_files_by_ids_for_audit(
        &self,
        ids: &[Uuid],
    ) -> HashMap<Uuid, file_storage_sdk::File> {
        if ids.is_empty() {
            return HashMap::new();
        }
        match self.store.list_files_by_ids(ids).await {
            Ok(files) => files.into_iter().map(|f| (f.file_id, f)).collect(),
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    file_ids = ?ids,
                    "cleanup: failed to load files for audit tenant attribution"
                );
                HashMap::new()
            }
        }
    }

    /// Delete one abandoned pending version row, clean up its backend blob,
    /// and -- if that leaves the parent file with no versions and a `NULL`
    /// `content_id` -- delete the now-permanently-orphaned `files` row too.
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
    /// pattern `sweep_expired_multipart_page`'s step already uses. Between
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
    /// `prefetched_file` is this candidate's parent `File`, if the caller
    /// already resolved it (`sweep_abandoned_pending_page` batch-loads every
    /// candidate's file in one round trip before its loop, instead of one
    /// `get_file` per candidate here); `None` when the batch load did not
    /// cover this `file_id` (e.g. it failed, or a direct unit-test call has
    /// no snapshot to hand in). Reused below both for this function's own
    /// audit row's `tenant_id` and, passed through unchanged, for
    /// `maybe_delete_orphaned_file` -- same read-elimination as
    /// `cleanup_expired_session_version_with_file`'s own `prefetched_file`.
    ///
    /// `pub` (rather than private) solely so a unit test can invoke it
    /// directly to exercise the narrow mid-flight interleaving window
    /// deterministically, without real concurrency -- mirroring
    /// [`Self::cleanup_expired_session_version`]'s reason for being `pub`
    /// on step 2's sibling race. Otherwise only called from
    /// `sweep_abandoned_pending_page`, one snapshot at a time.
    pub async fn delete_abandoned_pending_version(
        &self,
        file_id: Uuid,
        version_id: Uuid,
        size: i64,
        backend_id: &str,
        backend_path: &str,
        prefetched_file: Option<file_storage_sdk::File>,
    ) -> (usize, usize) {
        let file = prefetched_file;
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
            Ok(true) => {
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

                // Best-effort blob cleanup -- a failure here leaves an unreachable
                // orphan blob which is acceptable in P2.
                self.best_effort_delete(backend_id, backend_path).await;
                // Reuse the caller-supplied `prefetched_file` snapshot for
                // this function's own audit row, instead of letting
                // `orphan_candidate_file` fetch it a second time -- same
                // read-elimination as `cleanup_expired_session_version_with_file`'s.
                let files_deleted = self
                    .maybe_delete_orphaned_file(
                        file_id,
                        file,
                        "abandoned_pending_version_orphan_file",
                    )
                    .await;
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

    /// Second phase of sweep step 1: reclaim one batch of `files` rows that
    /// never received **any** version at all -- not even a `pending` one --
    /// and are therefore invisible to
    /// [`Self::sweep_abandoned_pending_page`]'s own zero-version-orphan
    /// reclaim just above, which only ever runs as a side effect of aging out
    /// a `file_versions` row that existed in the first place.
    ///
    /// Such a row is born when the merged `POST /files` create+plan path
    /// commits [`crate::domain::service::FileService::create_file_bare`]'s
    /// version-less `files` row and then never gets as far as inserting a
    /// version for it -- a process crash between that commit and
    /// `MultipartService::initiate_multipart_upload`'s
    /// `insert_pending_version`, or a `FileService::
    /// compensate_failed_multipart_initiate` that itself failed to delete the
    /// row (see that method's doc comment). Neither of those ever produces a
    /// `file_versions` row, so `sweep_abandoned_pending_page`'s
    /// `list_abandoned_pending_versions` query (keyed on a *version's* age)
    /// and `sweep_expired_multipart_page`'s `list_expired_multipart_uploads`
    /// (keyed on a *session's* expiry) can never select the file.
    ///
    /// `after` is this phase's tick-local keyset cursor (`(created_at,
    /// file_id)`) -- see [`Self::sweep_abandoned_pending_page`]'s doc for why
    /// it exists and how it advances (same rule here: always to the last
    /// candidate seen, regardless of outcome).
    ///
    /// Returns `(orphan_files_deleted, exhausted, next_after)` -- `exhausted`
    /// under the same rule as `sweep_abandoned_pending_page`'s (short batch,
    /// or the list query errored).
    async fn sweep_versionless_files_page(
        &self,
        grace_cutoff: OffsetDateTime,
        after: Option<(OffsetDateTime, Uuid)>,
    ) -> (usize, bool, Option<(OffsetDateTime, Uuid)>) {
        let candidates = match self
            .store
            .list_versionless_orphan_files(grace_cutoff, VERSIONLESS_SWEEP_BATCH, after)
            .await
        {
            Ok(files) => files,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "cleanup: failed to list versionless orphan files"
                );
                return (0, true, after);
            }
        };
        let exhausted = (candidates.len() as u64) < VERSIONLESS_SWEEP_BATCH;
        let next_after = candidates
            .last()
            .map(|f| (f.created_at, f.file_id))
            .or(after);

        let mut count = 0_usize;
        for file in candidates {
            let file_id = file.file_id;
            // `maybe_delete_orphaned_file` re-verifies zero-versions fresh
            // (via `orphan_candidate_file`'s own `list_versions` call) and
            // still checks `has_blocking_multipart_session` before deleting
            // -- the same guard `sweep_abandoned_pending_page`'s reclaim
            // above relies on -- so a version or an in-progress multipart
            // session that appears for this file after the list query above
            // cannot be destroyed out from under it.
            count += self
                .maybe_delete_orphaned_file(file_id, Some(file), "versionless_orphan_file")
                .await;
        }
        (count, exhausted, next_after)
    }

    /// After deleting a file's last abandoned pending version, check whether
    /// the parent `files` row is now a permanent zero-version orphan (no
    /// versions left **and** `content_id IS NULL`) and delete it too if so.
    ///
    /// The checks here are a cheap pre-filter run against a fresh (but
    /// pre-transaction) snapshot -- to skip the extra round-trip on the
    /// common case where the file still has other versions or content. The
    /// authoritative guard locks the `files` row first and re-runs the same
    /// checks (plus a fresh no-active-multipart-session check) fresh
    /// **inside** that lock, in the same transaction as the file delete
    /// ([`crate::domain::ports::CleanupStore::delete_orphan_file_with_event`]),
    /// so a version inserted or a multipart session started in the gap
    /// between this pre-check and that call cannot cause data loss: the
    /// delete simply aborts and the file (with its new version/session) is
    /// left untouched. This is a strict guarantee, not merely a narrowed
    /// window -- see `FileRepo::lock_for_update`'s and
    /// `Store::delete_orphan_file_with_event`'s doc comments for why the row
    /// lock closes it fully rather than shrinking it.
    ///
    /// Returns `1` if the file row was deleted, `0` otherwise.
    ///
    /// `prefetched_file` lets a caller that has already read this `File` row
    /// moments ago (for its own audit-row `tenant_id`, typically) hand it down
    /// so [`Self::orphan_candidate_file`] does not read it a third time for
    /// the same file -- pass `None` when no such snapshot is available.
    ///
    /// `reason` is recorded verbatim in both the audit row's `detail.reason`
    /// and the `file.deleted` event's `payload.reason` -- callers pass a
    /// string describing *how* this file was found to be a permanent
    /// zero-version orphan, since [`Self::orphan_candidate_file`]'s own check
    /// (no versions, `NULL` `content_id`) is identical regardless of which
    /// sweep phase got here.
    async fn maybe_delete_orphaned_file(
        &self,
        file_id: Uuid,
        prefetched_file: Option<file_storage_sdk::File>,
        reason: &str,
    ) -> usize {
        let Some(file) = self.orphan_candidate_file(file_id, prefetched_file).await else {
            return 0;
        };

        let audit = orphan_reconcile_audit(
            file_id,
            file.tenant_id,
            serde_json::json!({ "reason": reason }),
        );
        let event = Some(FileEvent {
            tenant_id: file.tenant_id,
            owner_id: file.owner_id,
            file_id: file.file_id,
            event_type: "file.deleted".to_owned(),
            payload: serde_json::json!({ "reason": reason }),
        });

        match self
            .store
            .delete_orphan_file_with_event(file_id, audit, event)
            .await
        {
            Ok(true) => {
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
    /// Split out of [`Self::maybe_delete_orphaned_file`] to keep it simple;
    /// see that method's doc for why a pre-transaction snapshot is safe.
    ///
    /// `remaining` (the version list) is always re-fetched fresh here
    /// regardless of `prefetched_file` -- that check is the entire point of
    /// this pre-check and a caller-supplied `File` snapshot says nothing
    /// about it. `prefetched_file`, when `Some`, stands in for this method's
    /// own `get_file` call for the `content_id`/`tenant_id`/`owner_id` fields
    /// only; it may be slightly older than a fresh read (taken before
    /// whatever version-row cleanup the caller just did), but per
    /// `maybe_delete_orphaned_file`'s doc that staleness cannot cause an
    /// incorrect delete -- only, in a rare race, one extra delete attempt the
    /// transactional guard safely turns into a no-op.
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
    /// direction. Split out of [`Self::orphan_candidate_file`] to keep it simple.
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

    /// Whether `file_id` has an active (`in_progress` or `completing`)
    /// multipart session that should block orphan-file deletion.
    ///
    /// `sweep_abandoned_pending_page` keys only on a pending version's age, so a
    /// multipart session that is legitimately still active can still have
    /// its backing version aged past the orphan grace window and reclaimed
    /// earlier in the same sweep pass. If [`Self::orphan_candidate_file`]'s
    /// caller went on to delete the file here too, the `files` FK's
    /// `ON DELETE CASCADE` would take the still-active
    /// `multipart_uploads` row with it, destroying a live upload with no
    /// error surfaced to the caller. `completing` blocks exactly like
    /// `in_progress` here, and regardless of lease status: a session
    /// mid-assembly under a live lease must not be destroyed out from under
    /// its completer, and even a `completing` session whose lease has
    /// expired is left alone -- reaping it is `sweep_expired_multipart_page`'s
    /// job, not this guard's. Returning `true` leaves the file for a
    /// later sweep instead -- once the session is aborted/completed (by
    /// `sweep_expired_multipart_page` or the user), a subsequent pass will find
    /// zero versions and no active session, and finish reclaiming it
    /// then. A lookup failure is treated as blocking (logged), erring toward
    /// not deleting.
    async fn has_blocking_multipart_session(&self, file_id: Uuid) -> bool {
        match self.store.has_active_multipart_for_file(file_id).await {
            Ok(blocking) => blocking,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    %file_id,
                    "cleanup: failed to check active multipart sessions while \
                     checking for orphaned file"
                );
                true
            }
        }
    }

    /// Abort one batch of in-progress multipart sessions whose `expires_at`
    /// has passed. `after` is this phase's tick-local keyset cursor
    /// (`(expires_at, upload_id)`) -- see
    /// [`Self::sweep_abandoned_pending_page`]'s doc for why it exists and how
    /// it advances.
    ///
    /// Returns `(sessions_aborted, orphan_files_reclaimed, exhausted,
    /// next_after)` -- the second tally counts files reclaimed here, not only
    /// by step 1 (see `run_sweep`'s step 2 comment and
    /// `cleanup_expired_session_version`'s doc); `exhausted` under the same
    /// rule as `sweep_abandoned_pending_page`'s.
    async fn sweep_expired_multipart_page(
        &self,
        now: OffsetDateTime,
        after: Option<(OffsetDateTime, Uuid)>,
    ) -> (usize, usize, bool, Option<(OffsetDateTime, Uuid)>) {
        let sessions = match self
            .store
            .list_expired_multipart_uploads(now, EXPIRED_MULTIPART_SWEEP_BATCH, after)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "cleanup: failed to list expired multipart uploads"
                );
                return (0, 0, true, after);
            }
        };
        let exhausted = (sessions.len() as u64) < EXPIRED_MULTIPART_SWEEP_BATCH;
        let next_after = sessions
            .last()
            .map(|s| (s.expires_at, s.upload_id))
            .or(after);

        // Same batch-first pattern as `sweep_abandoned_pending_page`: resolve
        // every candidate session's parent file in ONE round trip instead of
        // one `get_file` per session inside the loop below.
        let file_ids: Vec<Uuid> = {
            let mut seen = std::collections::HashSet::new();
            sessions
                .iter()
                .map(|s| s.file_id)
                .filter(|id| seen.insert(*id))
                .collect()
        };
        let files_by_id = self.load_files_by_ids_for_audit(&file_ids).await;

        let mut aborted_count = 0_usize;
        let mut files_count = 0_usize;
        for session in sessions {
            let prefetched_file = files_by_id.get(&session.file_id).cloned();
            let (aborted, files) = self
                .abort_expired_multipart_session(session, prefetched_file)
                .await;
            aborted_count += aborted;
            files_count += files;
        }
        (aborted_count, files_count, exhausted, next_after)
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
    async fn abort_expired_multipart_session(
        &self,
        session: MultipartUploadSession,
        prefetched_file: Option<file_storage_sdk::File>,
    ) -> (usize, usize) {
        // `prefetched_file` is this session's parent `File`, already
        // resolved by `sweep_expired_multipart_page`'s batch load before its loop
        // (`load_files_by_ids_for_audit`) -- thread it all the way down
        // through `cleanup_expired_session_version_with_file` to
        // `orphan_candidate_file`, instead of letting each of those three
        // spots fetch it independently. `None` (batch load did not cover this
        // `file_id`, e.g. it failed) falls back to `Uuid::nil()` below rather
        // than blocking the abort.
        let file = prefetched_file;
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
        match self
            .store
            .abort_multipart_upload(session.upload_id, abort_audit)
            .await
        {
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
            Err(e) => {
                tracing::warn!(error = ?e, upload_id = %session.upload_id,
                    "cleanup: failed to mark expired multipart upload as aborted");
                (0, 0)
            }
        }
    }

    /// Helper: abort the backend upload and delete the pending version row for
    /// an expired multipart session, then check whether that leaves the
    /// parent file a permanent zero-version orphan. Returns `1` if the orphan
    /// file was also reclaimed here, `0` otherwise.
    ///
    /// `pub` (rather than private) solely so a unit test can invoke it
    /// directly to exercise the narrow mid-flight interleaving window
    /// deterministically, without real concurrency: this function is
    /// otherwise only ever called from `abort_expired_multipart_session`
    /// after that method has already won the session CAS.
    ///
    /// The version row backing this session may already be gone by the time
    /// this runs: step 1 of the same sweep (`sweep_abandoned_pending_page`) only
    /// excludes an `in_progress` session with `expires_at > now` (still live)
    /// -- a `completing` session is excluded unconditionally instead, so this
    /// race is specific to `in_progress`. An *expired-but-still-`in_progress`*
    /// session's pending version can be reclaimed by step 1 before step 2
    /// (this method) ever sees it. When
    /// that happens the backend multipart upload handle must still be
    /// aborted -- otherwise it leaks (e.g. an incomplete S3 multipart upload
    /// and its uploaded parts) -- so the backend abort is attempted
    /// regardless of whether the version row is still present, falling back
    /// to the session's own stored `backend_id`/`backend_path`
    /// (`m20260924_000001_upload_flow_redesign`) when it is not, and only
    /// falling further back to the default backend and the recomputed
    /// deterministic `(file_id, version_id)` path for a legacy session that
    /// predates those columns -- see the implementation's own doc comment for
    /// the full precedence.
    ///
    /// This method finishes with the same `maybe_delete_orphaned_file` check
    /// `delete_abandoned_pending_version` runs after its own version delete,
    /// so the orphan reclaim is symmetric across both cleanup paths:
    /// whichever of the two (step 1's version reclaim, or this method) is the
    /// one to leave a file with zero versions and a `NULL` `content_id` also
    /// gets the chance to notice it. By the time this method runs,
    /// `abort_expired_multipart_session` has already won the session's own
    /// CAS to `aborted`, so `has_active_for_file` no longer blocks the
    /// reclaim -- unlike step 1's own attempt at the same file, which runs
    /// earlier in the same `run_sweep` pass while the session still looked
    /// `in_progress`/`completing` and correctly declined.
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
    /// production caller) has already read the same file, for its own
    /// "session aborted" audit row. `prefetched_file` lets a caller that
    /// already has a (possibly slightly stale) snapshot hand it down
    /// instead: reused here for the delete-audit `tenant_id`, and passed
    /// through unchanged to `maybe_delete_orphaned_file` so
    /// `orphan_candidate_file` can skip its own `get_file` too. A `None`
    /// (the public wrapper's case) makes this fetch fresh on its own,
    /// removing only the *redundant* reads a caller's snapshot would replace.
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
        // Resolve `(backend_id, backend_path)` with a three-way precedence --
        // see the doc comment above for why the backend abort must not be
        // skipped just because the version row is already reclaimed:
        //   1. The version row, when it is still there -- the freshest
        //      source (`migrate_backend` could in principle have moved it,
        //      though never for a still-pending version in practice).
        //   2. The session row's own `backend_id`/`backend_path`
        //      (`m20260924_000001_upload_flow_redesign`), when the version is
        //      already gone but the session was created after that migration
        //      -- the exact pair the upload was actually initiated against,
        //      whichever backend that was.
        //   3. The default backend plus a freshly recomputed deterministic
        //      path -- ONLY for a legacy session that predates both that
        //      migration and its backfill (no version row, no stored pair on
        //      the session either). Silently wrong for a legacy session whose
        //      upload was never on the default backend, but there is no
        //      surviving record of which backend it really was.
        let (backend_id, backend_path) = if let Some(v) = ver.as_ref() {
            (v.backend_id.clone(), v.backend_path.clone())
        } else if let (Some(backend_id), Some(backend_path)) =
            (session.backend_id.as_ref(), session.backend_path.as_ref())
        {
            (backend_id.clone(), backend_path.clone())
        } else {
            (
                self.backends.default_id().to_owned(),
                storage_layout::backend_path(session.file_id, session.version_id),
            )
        };
        self.backend_abort_multipart_best_effort(
            &backend_id,
            &backend_path,
            &session.backend_upload_handle,
            session.upload_id,
        )
        .await;

        // Best-effort: delete the pending version row (a no-op, matching
        // zero rows, when step 1 already reclaimed it above). Status-guarded:
        // only deletes if the row is still `pending`, so a version that a
        // racing `complete_multipart_upload` already flipped to `available`
        // (via `finalize_version`, ahead of its own session CAS) is left
        // untouched -- the DELETE simply matches zero rows. Reuse
        // `prefetched_file` when the caller already has it; otherwise fetch
        // it here -- this is the ONE read of the file this method performs
        // (the same snapshot, if present, is handed to
        // `maybe_delete_orphaned_file` below instead of read again there).
        let file = match prefetched_file {
            Some(file) => Some(file),
            None => self.load_file_for_audit(session.file_id).await,
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

        // The session is now `aborted` (the caller only reaches this method
        // after winning that CAS), so `has_active_for_file` no longer
        // blocks reclaiming a zero-version, NULL-content_id parent -- whether
        // the version was just deleted above, or already reclaimed earlier by
        // step 1's own path (which was correctly blocked while this session
        // still looked active). `file` (the same snapshot used for
        // `del_audit`'s tenant_id above) is handed down so
        // `orphan_candidate_file` does not re-fetch it.
        self.maybe_delete_orphaned_file(
            session.file_id,
            file,
            "abandoned_pending_version_orphan_file",
        )
        .await
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

    /// Delete files expired by a retention rule, one keyset page (by
    /// `file_id`) at a time -- so the sweep never materializes every file
    /// across every tenant at once, memory stays bounded regardless of
    /// deployment size, and a full-table scan can span as many ticks as it
    /// needs to.
    ///
    /// `all_rules` is loaded once per tick by [`Self::run_sweep`] (the rule
    /// set is small relative to the files) and handed down unchanged to every
    /// page. `after` is the persisted cross-tick cursor (see
    /// [`CleanupEngine::retention_cursor`]) -- unlike the other three sweep
    /// phases' cursors, this one is NOT reset every tick: a full scan can
    /// legitimately span many ticks on a large deployment, and restarting
    /// from `file_id` zero every tick would starve files later in keyset
    /// order.
    ///
    /// Returns `(files_deleted, exhausted, next_after)`:
    /// - a short page (fewer than [`RETENTION_SWEEP_BATCH`] rows) means the
    ///   whole table has now been scanned -- `exhausted = true`,
    ///   `next_after = None` (the next tick, or the next call once more rules
    ///   exist, starts over from the beginning);
    /// - a full page means more files remain -- `exhausted = false`,
    ///   `next_after = Some(..)` pointing past this page, so either a later
    ///   pass this same tick or (once the budget runs out) the next tick
    ///   resumes from there; deleting rows in this page does not shift that
    ///   window, since the next query filters `file_id > next_after`;
    /// - a query error leaves the cursor untouched (`next_after = after`) and
    ///   marks the phase `exhausted` for this tick -- retrying the exact same
    ///   page next tick rather than either losing the caller's place or
    ///   spinning on a persistent error within this tick.
    async fn sweep_retention_expiry_page(
        &self,
        now: OffsetDateTime,
        all_rules: &[crate::domain::policy::StoredRetentionRule],
        after: Option<Uuid>,
    ) -> (usize, bool, Option<Uuid>) {
        let batch = match self
            .store
            .list_all_files_for_sweep(after, RETENTION_SWEEP_BATCH)
            .await
        {
            Ok(batch) => batch,
            Err(e) => {
                tracing::warn!(error = ?e, "cleanup: failed to list files for retention sweep");
                return (0, true, after);
            }
        };
        if batch.is_empty() {
            return (0, true, None);
        }
        let exhausted = (batch.len() as u64) < RETENTION_SWEEP_BATCH;
        let next_after = batch.last().map(|f| f.file_id);
        let deleted = self.expire_batch(&batch, all_rules, now).await;
        (
            deleted,
            exhausted,
            if exhausted { None } else { next_after },
        )
    }

    /// Apply retention rules to one page of files. Returns the number deleted.
    async fn expire_batch(
        &self,
        batch: &[file_storage_sdk::File],
        all_rules: &[crate::domain::policy::StoredRetentionRule],
        now: OffsetDateTime,
    ) -> usize {
        // Fetch the custom metadata of every file on this page that actually
        // needs it in ONE query, instead of one query per file. Files whose
        // applicable rules carry no metadata criterion are not fetched at all
        // (see `needs_metadata`), so a deployment with only age/inactivity
        // rules -- the common case -- issues zero metadata queries per page,
        // and one with metadata rules issues exactly one.
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
                    // A file whose rules need metadata we could not read is
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
    /// Issues no queries of its own: [`Self::expire_batch`] prefetches the
    /// metadata every file on the page needs (see that method's doc for why
    /// this avoids a per-file query) and hands it in via `prefetched_metadata`.
    /// `None` there means that batch fetch failed, so a file whose rules need
    /// metadata is skipped rather than expired on data that could not be read.
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
        let applicable: Vec<&crate::domain::policy::StoredRetentionRule> = all_rules
            .iter()
            .filter(|r| rule_applies_to_file(r, file))
            .collect();

        if applicable.is_empty() {
            return 0;
        }

        // Fetch custom metadata only when some applicable rule actually has a
        // metadata criterion -- see this method's doc comment. `rule_matches`
        // treats an empty slice identically to "no matching metadata entry",
        // and it never reaches the `metadata` parameter at all for a rule
        // whose `body.metadata` is `None`, so this is semantically a no-op
        // for every rule that doesn't need it.
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

        // OR semantics: if any rule triggers, delete the file.
        let should_expire = applicable
            .iter()
            .any(|r| rule_matches(&r.body, file, metadata, now));

        if !should_expire {
            return 0;
        }

        self.expire_file(file, now).await
    }

    /// Delete one retention-expired file (DB row + backend blobs). Returns 1 if deleted.
    ///
    /// The version list used for backend-blob cleanup is collected by
    /// [`CleanupStore::delete_file_with_event_collecting_versions`] **inside
    /// the same transaction** as the delete itself, not by a separate
    /// pre-transaction `list_versions` call -- a version inserted
    /// concurrently (e.g. a still-live `presign_version` on a file that
    /// retention has just decided to expire) between an earlier read and
    /// this delete's commit would otherwise be cascade-removed without ever
    /// being queued for cleanup, permanently leaking its backend blob (the
    /// cleanup engine only ever looks at rows still in the database, and by
    /// then this one has none). See that method's doc comment for the full
    /// mechanism.
    async fn expire_file(&self, file: &file_storage_sdk::File, now: OffsetDateTime) -> usize {
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
            .delete_file_with_event_collecting_versions(&scope, file.file_id, audit, event)
            .await
        {
            Ok(DeletedFile {
                removed: true,
                versions,
            }) => {
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
            Ok(DeletedFile { removed: false, .. }) => {
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

    /// Delete one batch of expired `idempotency_keys` rows (`expires_at <=
    /// now`). No cursor is threaded through: each call deletes the rows it
    /// selects, so a later call this same tick (or the next tick) naturally
    /// sees the next-oldest backlog rather than the rows just removed.
    ///
    /// Returns `(rows_deleted, exhausted)`: `exhausted` once the batch came
    /// back shorter than [`EXPIRED_IDEMPOTENCY_SWEEP_BATCH`] or the delete
    /// itself errored (logged here).
    async fn sweep_idempotency_page(&self, now: OffsetDateTime) -> (u64, bool) {
        match self
            .store
            .delete_expired_idempotency_keys(now, EXPIRED_IDEMPOTENCY_SWEEP_BATCH)
            .await
        {
            Ok(deleted) => (deleted, deleted < EXPIRED_IDEMPOTENCY_SWEEP_BATCH),
            Err(e) => {
                tracing::warn!(error = ?e, "cleanup: failed to delete expired idempotency keys");
                (0, true)
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
