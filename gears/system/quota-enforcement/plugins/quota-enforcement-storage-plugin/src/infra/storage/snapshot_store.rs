//! The two snapshot reads: `read_quota_snapshot` for one applicable set (the
//! read evaluate-preview runs) and the paginated `bulk_read_quota_snapshot`.
//!
//! # One transaction per read
//!
//! A read selects its Quota rows, assembles their snapshots and creates the
//! period rows it may create in one transaction, in that order:
//!
//! 1. The Quota rows, under the caller's scope. On `PostgreSQL` the
//!    transaction is `REPEATABLE READ`, so this first statement fixes the
//!    snapshot every later read sees: a Quota row, its counter row and the
//!    expired holds that correct the counter all come from one state.
//! 2. One clock reading, shared by every Quota on the page.
//! 3. The snapshots. A consumption Quota whose current window has no row yet
//!    reads that window as zero, which is exactly what the row the read is
//!    about to create holds.
//! 4. Last, just before commit, the rows the I3 exception permits: the
//!    current window's row of each active consumption Quota within its
//!    validity window that lacks one. Nothing is settled and no event is
//!    emitted; closing the elapsed period belongs to the mutating operation
//!    that crosses the boundary. A Quota outside its window gets no row.
//!
//! Creating the rows last keeps the window in which a writer's own insert of
//! the same period could wait on this transaction down to the commit itself.
//!
//! # Retry
//!
//! The whole read runs again when the backend reports contention: a
//! `PostgreSQL` serialization failure (for example a conflicting period row
//! committed after this snapshot) or deadlock, or `SQLite`'s `SQLITE_BUSY` /
//! `SQLITE_BUSY_SNAPSHOT` (a transaction that read first cannot upgrade to
//! write after another connection committed). Reads are idempotent, so a
//! retry is safe; a read that keeps failing surfaces as unavailable.

use quota_enforcement_sdk::{
    ApplicableQuotas, PageRequest, PageResult, QuotaId, QuotaSnapshot, QuotaType, StorageError,
};
use sea_orm::DbErr;
use time::OffsetDateTime;
use toolkit_db::DbError;
use toolkit_db::secure::{DBRunner, ScopeError, TxConfig, TxIsolationLevel};
use toolkit_security::AccessScope;

use super::consumption_store::{
    LockedCounter, SqlConsumptionStore, TxError, lift, snapshot_of_locked, window_of,
};
use super::cursor;
use super::entity::quota;
use super::quota_mapping;
use super::repo::consumption_counter_repo as counter_repo;
use super::repo::quota_repo::{self, SnapshotPair};

/// Attempts one read gets before its contention error is returned.
const READ_ATTEMPTS: u32 = 3;

/// A page the transaction read: its snapshots, in `quota_id` order, and
/// whether more rows follow.
struct Page {
    snapshots: Vec<QuotaSnapshot>,
    more: bool,
}

// @cpt-flow:cpt-cf-quota-enforcement-flow-snapshot-read:p1
// @cpt-algo:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1
// @cpt-dod:cpt-cf-quota-enforcement-dod-snapshot-read-only:p1
impl SqlConsumptionStore {
    /// Every active Quota whose subject is in `applicable`, in `quota_id`
    /// order.
    ///
    /// # Errors
    ///
    /// As the contract documents for `read_quota_snapshot`.
    pub async fn read_quota_snapshot_impl(
        &self,
        scope: &AccessScope,
        applicable: &ApplicableQuotas,
    ) -> Result<Vec<QuotaSnapshot>, StorageError> {
        const OPERATION: &str = "read quota snapshot";
        let page = self
            .read_page(scope, vec![pair_of(applicable)], None, None)
            .await
            .map_err(|error| lift(OPERATION, error))?;
        Ok(page.snapshots)
    }

    /// One page of the Quotas `pairs` select, each once, after the cursor.
    ///
    /// # Errors
    ///
    /// As the contract documents for `bulk_read_quota_snapshot`.
    pub async fn bulk_read_quota_snapshot_impl(
        &self,
        scope: &AccessScope,
        pairs: &[ApplicableQuotas],
        page: PageRequest,
    ) -> Result<PageResult<QuotaSnapshot>, StorageError> {
        const OPERATION: &str = "bulk read quota snapshot";
        // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-resume
        let after = page
            .cursor
            .as_deref()
            .map(|cursor| cursor::decode(cursor).map_err(|_| StorageError::InvalidCursor))
            .transpose()?;
        // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-resume
        let limit = cursor::effective_limit(page.limit);
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-page
        let read = self
            .read_page(
                scope,
                pairs.iter().map(pair_of).collect(),
                after.map(QuotaId::as_uuid),
                Some(limit),
            )
            .await
            .map_err(|error| lift(OPERATION, error))?;
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-page
        // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-limit-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-limit
        // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-uniform
        // The cursor holds only the last `quota_id` returned; a page that
        // fits carries none.
        let next_cursor = if read.more {
            read.snapshots
                .last()
                .map(|snapshot| cursor::encode(snapshot.quota_id))
        } else {
            None
        };
        // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-uniform
        // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-limit
        // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-limit-if
        // @cpt-begin:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-return
        Ok(PageResult {
            items: read.snapshots,
            next_cursor,
        })
        // @cpt-end:cpt-cf-quota-enforcement-algo-snapshot-pagination:p1:inst-spg-return
    }

    /// Run the read's one transaction, again on contention.
    async fn read_page(
        &self,
        scope: &AccessScope,
        pairs: Vec<SnapshotPair>,
        after: Option<uuid::Uuid>,
        limit: Option<u32>,
    ) -> Result<Page, TxError> {
        let clock = std::sync::Arc::clone(&self.clock);
        self.db
            .transaction_with_retry_max(
                TxConfig {
                    isolation: Some(TxIsolationLevel::RepeatableRead),
                    access_mode: None,
                },
                READ_ATTEMPTS,
                db_err_of,
                move |tx| {
                    let scope = scope.clone();
                    let pairs = pairs.clone();
                    let clock = std::sync::Arc::clone(&clock);
                    Box::pin(async move {
                        read_in_tx(tx, &scope, &pairs, after, limit, || clock()).await
                    })
                },
            )
            .await
    }
}

/// The storage filter of one applicable set.
fn pair_of(applicable: &ApplicableQuotas) -> SnapshotPair {
    SnapshotPair {
        tenant_id: applicable.tenant_id.as_uuid(),
        metric: applicable.metric.as_str().to_owned(),
        subjects: applicable
            .subjects
            .iter()
            .map(|subject| {
                (
                    subject.projection_type.to_string(),
                    subject.subject_id.clone(),
                )
            })
            .collect(),
    }
}

/// One attempt: select, read the clock, assemble, then create the permitted
/// rows.
async fn read_in_tx(
    tx: &impl DBRunner,
    scope: &AccessScope,
    pairs: &[SnapshotPair],
    after: Option<uuid::Uuid>,
    limit: Option<u32>,
    now_of: impl FnOnce() -> OffsetDateTime,
) -> Result<Page, TxError> {
    // One row past the page says whether another page follows.
    let mut rows = quota_repo::find_snapshot_rows(
        tx,
        scope,
        pairs,
        after,
        limit.map(|limit| u64::from(limit) + 1),
    )
    .await?;
    let more = limit.is_some_and(|limit| rows.len() > limit as usize);
    if let Some(limit) = limit {
        rows.truncate(limit as usize);
    }
    // Read after the page query, so the snapshot is fixed first.
    let now = now_of();
    let mut snapshots = Vec::with_capacity(rows.len());
    let mut missing = Vec::new();
    for row in &rows {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-assemble
        let (snapshot, needs_row) = assemble(tx, scope, row, now).await?;
        snapshots.push(snapshot);
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-assemble
        if needs_row {
            missing.push(row);
        }
    }
    // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-lazy-if
    // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-lazy
    for row in missing {
        let quota = quota_mapping::row_to_quota(row.clone())?;
        counter_repo::insert_current_period_of(tx, scope, row, &window_of(&quota, now), now)
            .await?;
    }
    // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-lazy
    // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-lazy-if
    Ok(Page { snapshots, more })
}

/// One Quota's snapshot, and whether the read may create its current row:
/// an active consumption Quota within its validity window whose current
/// window has no row yet.
async fn assemble(
    tx: &impl DBRunner,
    scope: &AccessScope,
    row: &quota::Model,
    now: OffsetDateTime,
) -> Result<(QuotaSnapshot, bool), TxError> {
    let quota = quota_mapping::row_to_quota(row.clone())?;
    if quota.quota_type == QuotaType::Consumption {
        let latest = counter_repo::find_latest(tx, scope, quota.id.as_uuid()).await?;
        let current = latest
            .as_ref()
            .is_some_and(|row| row.period_start <= now && now < row.period_end);
        let valid = quota
            .validity_window
            .is_none_or(|window| window.contains(now));
        let counter = LockedCounter::consumption(&quota, latest, now);
        let snapshot = snapshot_of_locked(tx, scope, &quota, &counter, now).await?;
        Ok((snapshot, valid && !current))
    } else {
        let row = counter_repo::find_allocation(tx, scope, quota.id.as_uuid()).await?;
        let counter = LockedCounter::allocation(row.map(|row| row.in_flight));
        let snapshot = snapshot_of_locked(tx, scope, &quota, &counter, now).await?;
        Ok((snapshot, false))
    }
}

/// The database error inside a read's failure, for the contention check.
fn db_err_of(error: &TxError) -> Option<&DbErr> {
    match error {
        TxError::Scope(ScopeError::Db(db)) | TxError::Db(DbError::Sea(db)) => Some(db),
        _ => None,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "snapshot_store_tests.rs"]
mod tests;
