use std::convert::Infallible;
use std::sync::Arc;

use sea_orm::{FromQueryResult, Statement};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::super::OutboxError;
use super::super::core::Outbox;
use super::super::prioritizer::SharedPrioritizer;
use super::super::store::OutboxStore;
use super::super::taskward::{Directive, WorkerAction};
use crate::Db;

#[derive(Debug, FromQueryResult)]
struct DirtyPartitionRow {
    partition_id: i64,
}

/// Discover pending partitions from the incoming table and populate
/// the in-memory dirty set. Used at startup for eager reconciliation,
/// by sequencers after a post-commit flush, and by the cold reconciler (poker).
pub async fn reconcile_dirty(
    outbox: &Outbox,
    db: &Db,
    prioritizer: &SharedPrioritizer,
) -> Result<(), OutboxError> {
    let conn = db.sea_internal();
    debug_assert_eq!(conn.get_database_backend(), outbox.statements().backend());
    let store = OutboxStore::new(outbox.statements());

    let rows = DirtyPartitionRow::find_by_statement(Statement::from_sql_and_values(
        store.backend(),
        store.discover_dirty_partitions(),
        [],
    ))
    .all(&conn)
    .await?;

    let mut found = 0u64;
    for row in rows {
        prioritizer.push_dirty(row.partition_id);
        found += 1;
    }

    if found > 0 {
        tracing::debug!(found, "outbox: discovered dirty partitions");
    }
    Ok(())
}

/// Cold reconciler as a `WorkerAction` — periodically discovers pending
/// partitions from the incoming table, populates the dirty set, and
/// wakes the sequencer. Driven by `WorkerBuilder::pacing(idle_interval)`.
pub struct ColdReconciler {
    pub outbox: Arc<Outbox>,
    pub db: Db,
    pub prioritizer: Arc<SharedPrioritizer>,
}

impl WorkerAction for ColdReconciler {
    type Payload = ();
    /// Deliberately infallible, unlike the outbox's other background workers.
    ///
    /// They report failures so the worker loop escalates its backoff, because
    /// theirs delay something auxiliary. This one is the pipeline's liveness
    /// net: it discovers work enqueued by *another* instance even when there
    /// are no local flushes, since that instance's notification never reaches
    /// this process. Delaying its retry delays that discovery.
    ///
    /// And escalating would buy nothing measurable anyway - its `retry_max` is
    /// one minute and its `idle_interval` is one minute, so the backoff caps at
    /// the pace it already keeps. The failure reaches the log either way.
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Self::Error> {
        if let Err(e) = reconcile_dirty(&self.outbox, &self.db, &self.prioritizer).await {
            warn!(error = %e, "cold reconciler: failed to discover dirty partitions");
        }
        Ok(Directive::idle())
    }
}
