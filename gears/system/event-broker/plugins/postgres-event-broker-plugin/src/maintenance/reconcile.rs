//! Reconcile (`docs/DESIGN.md` §4.9): repairs what the cross-check found on
//! one partition, after reading the rows and the catalog again under the
//! extender's and the detacher's locks, so it never repairs what a job has
//! since put right.

use std::collections::HashMap;
use std::sync::Arc;

use sqlx::{AssertSqlSafe, PgPool};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};

use super::catalog;
use super::classify::{self, Class, Loudness, MediumDown, lock_timeout_for};
use super::crosscheck::{self, Case, Finding, Scope, Stats};
use super::prioritizer::{Due, Key, Taken};
use super::registry::Layout;
use crate::names;
use crate::shared::Shared;

/// Names the job in logs and worker names.
pub const NAME: &str = "reconcile";

/// Repairs the drift the cross-check found, one partition per pass.
pub struct Reconcile {
    shared: Arc<Shared>,
    pool: PgPool,
}

/// What one run found.
enum Reconciled {
    /// Repaired what could be; the layout as committed, and what no repair
    /// exists for.
    Repaired(Layout, Vec<Finding>),
    /// Another instance holds the partition's lock.
    Busy,
    /// The partition is gone.
    Gone,
}

impl Reconcile {
    /// The reconcile of one instance.
    pub fn new(shared: Arc<Shared>, pool: PgPool) -> Self {
        Self { shared, pool }
    }

    async fn reconcile(&self, key: Key, attempt: u32) -> Result<Reconciled, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::raw_sql(AssertSqlSafe(format!(
            "SET LOCAL lock_timeout = '{}ms'; SET LOCAL statement_timeout = '10s'",
            lock_timeout_for(attempt).as_millis()
        )))
        .execute(&mut *tx)
        .await?;
        // The extender's lock, and the detacher's: no segment of the
        // partition changes between reading the rows and the catalog.
        if !catalog::try_xact_lock(&mut tx, &catalog::segment_lock(key)).await? {
            return Ok(Reconciled::Busy);
        }
        if !catalog::try_xact_lock(&mut tx, &catalog::detach_lock(key)).await? {
            tx.commit().await?;
            return Ok(Reconciled::Busy);
        }
        let (topic, partition) = key;
        let row: Option<PartitionRow> = sqlx::query_as(
            "SELECT p.hwm, r.floor FROM evbk__partition p JOIN evbk__retention r USING (topic_uuid, partition) \
             WHERE p.topic_uuid = $1 AND p.partition = $2",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(PartitionRow { hwm, floor }) = row else {
            // Ends the transaction, and with it the lock, now rather than
            // when the pool next pings the connection.
            tx.commit().await?;
            return Ok(Reconciled::Gone);
        };
        let rows = crosscheck::rows(&mut tx, Scope::Partition(key)).await?;
        let relations = crosscheck::relations(&mut tx, Scope::Partition(key)).await?;
        let stats = HashMap::from([(key, Stats { hwm, floor })]);
        let mut left = Vec::new();
        for finding in crosscheck::compare(&rows, &relations, &stats) {
            let table = &finding.segment;
            match finding.case {
                // The extender makes it again.
                Case::Unbuilt => {
                    sqlx::query("DELETE FROM evbk__segment WHERE table_name = $1")
                        .bind(table)
                        .execute(&mut *tx)
                        .await?;
                }
                Case::Unrecorded => {
                    let bound = relations
                        .iter()
                        .find(|r| r.name == *table)
                        .and_then(|r| r.bound);
                    match bound {
                        Some(range) => catalog::insert_open(&mut tx, key, range, table).await?,
                        None => left.push(finding),
                    }
                }
                Case::Leftover => {
                    sqlx::raw_sql(AssertSqlSafe(format!("DROP TABLE {table}")))
                        .execute(&mut *tx)
                        .await?;
                }
                Case::Lost | Case::Gap => left.push(finding),
                // The detacher's, or report only.
                _ => {}
            }
        }
        let segments = catalog::live_segments(&mut tx, key).await?;
        tx.commit().await?;
        Ok(Reconciled::Repaired(Layout::new(&segments), left))
    }
}

/// What a run reads of its partition.
#[derive(sqlx::FromRow)]
struct PartitionRow {
    hwm: i64,
    floor: i64,
}

impl WorkerAction for Reconcile {
    type Payload = ();
    type Error = MediumDown;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, MediumDown> {
        let hub = &self.shared.maintenance;
        let lease = match hub.reconcile.take() {
            Taken::Lease(lease) => lease,
            Taken::Until(at) => return Ok(Directive::sleep(at - Instant::now())),
            Taken::Empty => return Ok(Directive::idle()),
        };
        // Stopping: the dropped lease puts the key back.
        if cancel.is_cancelled() {
            return Ok(Directive::idle());
        }
        let key = lease.key();
        match self.reconcile(key, lease.attempt()).await {
            Ok(Reconciled::Repaired(layout, left)) => {
                hub.registry.replace(key, layout);
                if left.is_empty() {
                    lease.done();
                } else {
                    // The cross-check reported it; failing keeps the key until
                    // it is reported stuck.
                    lease.failed(&format!("no repair for {left:?}"));
                }
            }
            Ok(Reconciled::Busy) => lease.busy(),
            Ok(Reconciled::Gone) => {
                hub.registry.forget(key);
                lease.gone();
            }
            Err(err) => match classify::class(&err, lease.attempt()) {
                Class::Medium => {
                    lease.deferred();
                    return Err(MediumDown::new(NAME, &err));
                }
                Class::Gone => {
                    hub.registry.forget(key);
                    lease.gone();
                }
                Class::Contended | Class::Stale => {
                    classify::log(NAME, key, &err, Loudness::Quiet);
                    lease.contended(&err.to_string());
                }
                Class::Failed(loudness) => {
                    classify::log(NAME, key, &err, loudness);
                    lease.failed(&err.to_string());
                }
                Class::Bug | Class::Permanent => {
                    classify::log(NAME, key, &err, Loudness::Error);
                    lease.failed(&err.to_string());
                }
            },
        }
        Ok(match hub.reconcile.peek() {
            Due::Now => Directive::proceed(),
            Due::At(at) => Directive::sleep(at - Instant::now()),
            Due::Never => Directive::idle(),
        })
    }
}
