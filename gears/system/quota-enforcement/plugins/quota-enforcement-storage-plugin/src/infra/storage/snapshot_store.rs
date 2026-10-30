//! The two snapshot reads: `read_quota_snapshot` for one applicable set (the
//! read evaluate-preview runs) and the paginated `bulk_read_quota_snapshot`.
//!
//! # One transaction per read
//!
//! A read selects its Quota rows and assembles their snapshots in one
//! transaction, in that order:
//!
//! 1. The Quota rows, under the caller's scope. On `PostgreSQL` the
//!    transaction is `REPEATABLE READ`, so this first statement fixes the
//!    snapshot every later read sees: a Quota row, its counter row and the
//!    expired holds that correct the counter all come from one state.
//! 2. One clock reading, shared by every Quota on the page.
//! 3. The snapshots, from three statements per thousand Quotas whatever the
//!    page holds: their current period rows, their allocation rows, and the
//!    expired holds those rows still owe. A consumption Quota whose current
//!    window has no row yet reads that window as zero; the next mutating
//!    operation creates the row.
//!
//! A read writes nothing and takes no row lock (I3), so it never waits on a
//! writer and a writer never waits on it. On `PostgreSQL` the transaction is
//! also `READ ONLY`, so a write on this path fails rather than slipping in.

use std::collections::HashMap;

use quota_enforcement_sdk::{
    ApplicableQuotas, PageRequest, PageResult, Quota, QuotaId, QuotaSnapshot, QuotaType,
    StorageError,
};
use time::OffsetDateTime;
use toolkit_db::secure::{DBRunner, TxAccessMode, TxConfig, TxIsolationLevel};
use toolkit_security::AccessScope;

use uuid::Uuid;

use super::consumption_store::{LockedCounter, SqlConsumptionStore, TxError, lift, snapshot_from};
use super::cursor;
use super::quota_mapping;
use super::repo::consumption_counter_repo as counter_repo;
use super::repo::lease_repo;
use super::repo::quota_repo::{self, SnapshotPair};

/// Quota ids one statement binds: far below every backend's parameter limit.
const IDS_PER_STATEMENT: usize = 1_000;

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

    /// Run the read's one transaction.
    async fn read_page(
        &self,
        scope: &AccessScope,
        pairs: Vec<SnapshotPair>,
        after: Option<uuid::Uuid>,
        limit: Option<u32>,
    ) -> Result<Page, TxError> {
        let clock = std::sync::Arc::clone(&self.clock);
        let scope = scope.clone();
        self.db
            .transaction_ref_mapped_with_config(
                TxConfig {
                    isolation: Some(TxIsolationLevel::RepeatableRead),
                    access_mode: Some(TxAccessMode::ReadOnly),
                },
                move |tx| {
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

/// One attempt: select, read the clock, assemble.
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
    let quotas = rows
        .into_iter()
        .map(quota_mapping::row_to_quota)
        .collect::<Result<Vec<_>, _>>()?;
    let mut snapshots = Vec::with_capacity(quotas.len());
    for chunk in quotas.chunks(IDS_PER_STATEMENT) {
        // @cpt-begin:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-assemble
        snapshots.extend(assemble(tx, scope, chunk, now).await?);
        // @cpt-end:cpt-cf-quota-enforcement-flow-snapshot-read:p1:inst-snp-assemble
    }
    Ok(Page { snapshots, more })
}

/// The snapshots of `quotas`, in their order, from at most three statements.
async fn assemble(
    tx: &impl DBRunner,
    scope: &AccessScope,
    quotas: &[Quota],
    now: OffsetDateTime,
) -> Result<Vec<QuotaSnapshot>, TxError> {
    let ids_of = |kind: QuotaType| -> Vec<Uuid> {
        quotas
            .iter()
            .filter(|quota| quota.quota_type == kind)
            .map(|quota| quota.id.as_uuid())
            .collect()
    };
    let (consumption, allocation) = (
        ids_of(QuotaType::Consumption),
        ids_of(QuotaType::Allocation),
    );
    let mut current = HashMap::new();
    if !consumption.is_empty() {
        for row in counter_repo::find_current_of(tx, scope, &consumption, now).await? {
            current.insert(row.quota_id, row);
        }
    }
    let mut in_flight = HashMap::new();
    if !allocation.is_empty() {
        for row in counter_repo::find_allocations_of(tx, scope, &allocation).await? {
            in_flight.insert(row.quota_id, row.in_flight);
        }
    }
    let counters: Vec<LockedCounter> = quotas
        .iter()
        .map(|quota| {
            let id = quota.id.as_uuid();
            if quota.quota_type == QuotaType::Consumption {
                LockedCounter::consumption(quota, current.remove(&id), now)
            } else {
                LockedCounter::allocation(in_flight.get(&id).copied())
            }
        })
        .collect();

    // A read may not write (I3), so it subtracts the expired holds nobody has
    // returned yet rather than returning them (I4).
    let ids: Vec<Uuid> = quotas.iter().map(|quota| quota.id.as_uuid()).collect();
    let periods: Vec<Uuid> = counters
        .iter()
        .filter_map(LockedCounter::period_id)
        .collect();
    let mut owed: HashMap<(Uuid, Option<Uuid>), u64> = HashMap::new();
    for hold in lease_repo::unreturned_expired_holds_of(tx, scope, &ids, &periods, now).await? {
        let sum = owed.entry((hold.quota_id, hold.period_id)).or_default();
        *sum = sum.saturating_add(u64::try_from(hold.held_amount).unwrap_or(0));
    }
    Ok(quotas
        .iter()
        .zip(&counters)
        .map(|(quota, counter)| {
            let key = (quota.id.as_uuid(), counter.period_id());
            snapshot_from(quota, counter, owed.get(&key).copied().unwrap_or(0), now)
        })
        .collect())
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "snapshot_store_tests.rs"]
mod tests;
