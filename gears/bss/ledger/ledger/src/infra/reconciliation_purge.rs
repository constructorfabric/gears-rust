//! `RetiredRunPurger` — the reconciliation tick's only delete path on
//! `ledger_reconciliation_run`: reclaim the uneventful runs of tenants the
//! platform registry reports as soft-deleted (see
//! [`crate::infra::tenant_lifecycle`] for why those rows accumulate at all).
//!
//! Bounded three ways so a multi-GB backlog drains without any single tick
//! becoming a WAL event: a per-statement batch, a per-tick row budget
//! (`recon.purge_max_rows_per_tick`, `0` = off) and a per-tick tenant budget
//! (`recon.purge_max_tenants_per_tick`). A rotation cursor carries progress
//! across ticks so a deleted set larger than one tick's budget is covered in
//! rotation instead of re-grinding its head.

use std::sync::Mutex;

use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::model::RepoError;
use crate::infra::storage::repo::ReconciliationRunRepo;

/// Reconciliation runs deleted per statement. Postgres has no `DELETE … LIMIT`,
/// so the purge picks a batch of `run_id`s then deletes them by key; this
/// bounds one statement's WAL while keeping the round-trip count low, and keeps
/// the `run_id IN (…)` bind list far below the 65,535-parameter ceiling.
pub(crate) const PURGE_BATCH_ROWS: u64 = 5_000;

/// The one storage operation the purge needs: delete up to `limit` uneventful
/// runs of one tenant, returning how many went. Implemented by
/// [`ReconciliationRunRepo::purge_uneventful_runs`]; a port so the rotation and
/// budget logic is testable without a database.
#[async_trait]
pub(crate) trait UneventfulRunStore: Send + Sync {
    /// See [`ReconciliationRunRepo::purge_uneventful_runs`].
    async fn purge_uneventful_runs(&self, tenant: Uuid, limit: u64) -> Result<u64, RepoError>;
}

#[async_trait]
impl UneventfulRunStore for ReconciliationRunRepo {
    async fn purge_uneventful_runs(&self, tenant: Uuid, limit: u64) -> Result<u64, RepoError> {
        ReconciliationRunRepo::purge_uneventful_runs(self, tenant, limit).await
    }
}

/// What one [`RetiredRunPurger::purge`] pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PurgeOutcome {
    /// Rows deleted across every tenant visited.
    pub deleted: u64,
    /// Tenants visited (drained, budget-stopped, or failed).
    pub visited: usize,
    /// Tenants whose purge statement failed and were skipped.
    pub failed: usize,
}

/// Budgeted, rotating purge over the deleted tenants of one tick.
pub(crate) struct RetiredRunPurger {
    store: Box<dyn UneventfulRunStore>,
    max_rows_per_tick: u64,
    max_tenants_per_tick: usize,
    batch_rows: u64,
    /// The last tenant a pass visited. The next pass resumes strictly after it
    /// (ids are visited in sorted order, wrapping around).
    cursor: Mutex<Option<Uuid>>,
}

impl RetiredRunPurger {
    /// Build a purger with the tick budgets from `ReconConfig` and the
    /// per-statement batch size (normally [`PURGE_BATCH_ROWS`]).
    pub(crate) fn new(
        store: Box<dyn UneventfulRunStore>,
        max_rows_per_tick: u64,
        max_tenants_per_tick: usize,
        batch_rows: u64,
    ) -> Self {
        Self {
            store,
            max_rows_per_tick,
            max_tenants_per_tick,
            batch_rows: batch_rows.max(1),
            cursor: Mutex::new(None),
        }
    }

    /// Whether the purge is switched on (`purge_max_rows_per_tick > 0`).
    pub(crate) fn enabled(&self) -> bool {
        self.max_rows_per_tick > 0
    }

    /// Reclaim uneventful runs of `deleted` tenants within this tick's budgets.
    ///
    /// Fire-and-forget: a failing tenant is logged, counted in
    /// [`PurgeOutcome::failed`] and skipped — reclaiming storage must never
    /// fail a reconciliation tick. The cursor moves past every tenant visited
    /// (failing or budget-stopped included), so neither a bad tenant nor a
    /// large backlog can stall the rotation; the remainder is retried on the
    /// next lap.
    pub(crate) async fn purge(&self, deleted: &[Uuid]) -> PurgeOutcome {
        let mut outcome = PurgeOutcome::default();
        let mut budget = self.max_rows_per_tick;
        if budget == 0 || deleted.is_empty() || self.max_tenants_per_tick == 0 {
            return outcome;
        }
        // Sorted so "resume strictly after the cursor" is meaningful across
        // ticks even as the deleted set grows.
        let mut sorted: Vec<Uuid> = deleted.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        let start = self
            .cursor()
            .map_or(0, |c| sorted.partition_point(|t| *t <= c));
        let visit = self.max_tenants_per_tick.min(sorted.len());
        let mut passed: Option<Uuid> = None;

        for tenant in sorted.iter().copied().cycle().skip(start).take(visit) {
            if budget == 0 {
                break;
            }
            outcome.visited += 1;
            loop {
                let batch = budget.min(self.batch_rows);
                match self.store.purge_uneventful_runs(tenant, batch).await {
                    Ok(n) => {
                        outcome.deleted = outcome.deleted.saturating_add(n);
                        budget = budget.saturating_sub(n);
                        // A short batch means the tenant is drained; a spent
                        // budget ends the tick. Either way, move on.
                        if n < batch || budget == 0 {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "bss-ledger",
                            %tenant,
                            error = %e,
                            "recon tick: purge of deleted-tenant reconciliation runs failed; skipping tenant"
                        );
                        outcome.failed += 1;
                        break;
                    }
                }
            }
            // The cursor passes every tenant visited — drained, stopped by the
            // row budget, or failed — so one large backlog cannot monopolise
            // successive ticks; its remainder is picked up on the next lap.
            passed = Some(tenant);
        }

        if let Some(last) = passed {
            self.set_cursor(last);
        }
        outcome
    }

    /// Read the rotation cursor. A poisoned lock is treated as "no cursor"
    /// (restart the rotation) — the purge is idempotent, so losing the
    /// position costs a repeated pass over already-drained tenants, never
    /// correctness.
    fn cursor(&self) -> Option<Uuid> {
        self.cursor.lock().ok().and_then(|c| *c)
    }

    /// Advance the rotation cursor. See [`Self::cursor`] on poisoning.
    fn set_cursor(&self, tenant: Uuid) {
        if let Ok(mut cursor) = self.cursor.lock() {
            *cursor = Some(tenant);
        }
    }
}

#[cfg(test)]
#[path = "reconciliation_purge_tests.rs"]
mod tests;
