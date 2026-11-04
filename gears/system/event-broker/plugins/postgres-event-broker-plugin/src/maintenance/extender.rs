//! The extender (`docs/DESIGN.md` §4.9): keeps segments ahead of a
//! partition's head, so an append always finds one.

use std::sync::Arc;

use sqlx::{AssertSqlSafe, PgPool};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};

use super::catalog::{self, Segment, SegmentState};
use super::classify::{self, Class, Loudness, MediumDown, lock_timeout_for};
use super::prioritizer::{Due, Key, Taken};
use super::registry::Layout;
use crate::names;
use crate::segment::{self, Observed};
use crate::shared::Shared;

/// Names the job in logs and worker names.
pub const NAME: &str = "extender";

/// Creates the segments a partition lacks, one partition per pass.
pub struct Extender {
    shared: Arc<Shared>,
    pool: PgPool,
}

/// What one run found.
enum Extended {
    /// The partition has room; its layout as committed.
    Done(Layout),
    /// Another instance is extending it, and makes what it needs.
    Busy,
    /// The partition is gone.
    Gone,
}

impl Extender {
    /// The extender of one instance.
    pub fn new(shared: Arc<Shared>, pool: PgPool) -> Self {
        Self { shared, pool }
    }

    async fn extend(&self, key: Key, attempt: u32) -> Result<Extended, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::raw_sql(AssertSqlSafe(format!(
            "SET LOCAL lock_timeout = '{}ms'; SET LOCAL statement_timeout = '10s'",
            lock_timeout_for(attempt).as_millis()
        )))
        .execute(&mut *tx)
        .await?;
        if !catalog::try_xact_lock(&mut tx, &catalog::segment_lock(key)).await? {
            return Ok(Extended::Busy);
        }
        let (topic, partition) = key;
        let row: Option<PartitionRow> = sqlx::query_as(
            "SELECT hwm, table_name AS parent FROM evbk__partition \
             WHERE topic_uuid = $1 AND partition = $2",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(PartitionRow { hwm, parent }) = row else {
            // Ends the transaction, and with it the lock, now rather than
            // when the pool next pings the connection.
            tx.commit().await?;
            return Ok(Extended::Gone);
        };
        let mut segments = catalog::live_segments(&mut tx, key).await?;
        let ranges: Vec<_> = segments.iter().map(|s| (s.first, s.end)).collect();
        let span = segment::span(self.shared.settings.segment, observed(&segments));
        for (first, end) in segment::ahead(hwm, &ranges, span) {
            let name = names::segment(&parent, first);
            // `ATTACH` takes SHARE UPDATE EXCLUSIVE on the parent, so appends
            // and reads go on; `PARTITION OF` would take ACCESS EXCLUSIVE.
            sqlx::raw_sql(AssertSqlSafe(format!(
                "CREATE TABLE {name} (LIKE {parent} INCLUDING ALL); \
                 ALTER TABLE {parent} ATTACH PARTITION {name} FOR VALUES FROM ({first}) TO ({end})"
            )))
            .execute(&mut *tx)
            .await?;
            catalog::insert_open(&mut tx, key, (first, end), &name).await?;
            segments.push(Segment::open(first, end));
        }
        tx.commit().await?;
        Ok(Extended::Done(Layout::new(&segments)))
    }
}

/// What a run reads of its partition.
#[derive(sqlx::FromRow)]
struct PartitionRow {
    hwm: i64,
    /// The partitioned table.
    parent: String,
}

impl WorkerAction for Extender {
    type Payload = ();
    type Error = MediumDown;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, MediumDown> {
        let hub = &self.shared.maintenance;
        let lease = match hub.extend.take() {
            Taken::Lease(lease) => lease,
            Taken::Until(at) => return Ok(Directive::sleep(at - Instant::now())),
            Taken::Empty => return Ok(Directive::idle()),
        };
        // Stopping: the dropped lease puts the key back.
        if cancel.is_cancelled() {
            return Ok(Directive::idle());
        }
        let key = lease.key();
        match self.extend(key, lease.attempt()).await {
            Ok(Extended::Done(layout)) => {
                hub.registry.replace(key, layout);
                lease.done();
            }
            Ok(Extended::Busy) => lease.busy(),
            Ok(Extended::Gone) => {
                hub.registry.forget(key);
                lease.gone();
            }
            Err(err) => {
                // A table already holds the name or a range this run meant
                // to create, and no row names it. Reconcile checks again
                // under its locks and adopts it at once, instead of after
                // two cross-checks.
                if let sqlx::Error::Database(db) = &err
                    && matches!(db.code().as_deref(), Some("42P07" | "42P17"))
                {
                    hub.reconcile.offer(key, ());
                }
                match classify::class(&err, lease.attempt()) {
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
                }
            }
        }
        Ok(match hub.extend.peek() {
            Due::Now => Directive::proceed(),
            Due::At(at) => Directive::sleep(at - Instant::now()),
            Due::Never => Directive::idle(),
        })
    }
}

/// The rate between the ends of the two newest sealed segments of a
/// partition, for a `period` rule: no query beyond the rows the extender
/// reads anyway.
fn observed(segments: &[Segment]) -> Option<Observed> {
    let mut sealed = segments
        .iter()
        .filter(|s| s.state == SegmentState::Sealed)
        .filter_map(|s| s.newest_at.map(|at| (s.end - 1, at)))
        .rev();
    let (to, to_at) = sealed.next()?;
    let (from, from_at) = sealed.next()?;
    let millis = (to_at - from_at).num_milliseconds();
    #[allow(
        clippy::cast_precision_loss,
        reason = "milliseconds between two segments are far inside f64's exact range"
    )]
    let seconds = millis as f64 / 1000.0;
    Some(Observed {
        events: to - from,
        seconds,
    })
}
