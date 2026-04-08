use std::convert::Infallible;
use std::sync::Arc;

use sea_orm::{ConnectionTrait, FromQueryResult, Statement};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::super::core::Outbox;
use super::super::dialect::Dialect;
use super::super::prioritizer::SharedPrioritizer;
use super::super::taskward::{Directive, WorkerAction};
use crate::Db;

#[derive(Debug, FromQueryResult)]
struct DirtyPartitionRow {
    partition_id: i64,
}

/// Discover pending partitions from the incoming table and populate
/// the in-memory dirty set. Used at startup for eager reconciliation
/// and by the cold reconciler (poker) loop.
pub async fn reconcile_dirty(outbox: &Outbox, db: &Db, prioritizer: &SharedPrioritizer) {
    let conn = db.sea_internal();
    let backend = conn.get_database_backend();
    let dialect = Dialect::from(backend);

    let rows = match DirtyPartitionRow::find_by_statement(Statement::from_sql_and_values(
        backend,
        dialect.discover_dirty_partitions(),
        [],
    ))
    .all(&conn)
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "cold reconciler: failed to discover dirty partitions");
            return;
        }
    };

    let mut found = 0u64;
    for row in rows {
        prioritizer.push_dirty(row.partition_id);
        found += 1;
    }

    if found > 0 {
        tracing::debug!(found, "cold reconciler: discovered dirty partitions");
        outbox.flush();
    }
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
    type Error = Infallible;

    async fn execute(&mut self, _cancel: &CancellationToken) -> Result<Directive, Self::Error> {
        reconcile_dirty(&self.outbox, &self.db, &self.prioritizer).await;
        Ok(Directive::idle())
    }
}
