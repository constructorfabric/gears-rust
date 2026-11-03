//! The sealer (`docs/DESIGN.md` §4.9): seals each full open segment of a
//! partition with its table's size on disk and its last event's time, so
//! retention can weigh and age whole segments. A segment's size is final
//! once it is full: its table is insert-only.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{AssertSqlSafe, PgPool};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};

use super::catalog;
use super::classify::{self, Class, Loudness, MediumDown};
use super::prioritizer::{Due, Key, Taken};
use super::registry::Layout;
use crate::names;
use crate::shared::Shared;

/// Names the job in logs and worker names.
pub const NAME: &str = "sealer";

/// Seals the full open segments of a partition, one partition per pass.
pub struct Sealer {
    shared: Arc<Shared>,
    pool: PgPool,
}

/// What a run reads of its partition first.
#[derive(sqlx::FromRow)]
struct PartitionRow {
    hwm: i64,
    /// Whether the topic has any retention bound.
    bounded: bool,
}

/// A full open segment.
#[derive(sqlx::FromRow)]
struct FullRow {
    first: i64,
    /// Its table.
    segment: String,
}

/// What a sealed segment records.
#[derive(sqlx::FromRow)]
struct Measured {
    /// `pg_total_relation_size` of its table: heap, TOAST and indexes.
    bytes: i64,
    /// Its last event's `sequence_time`; `None` with no event left.
    newest_at: Option<DateTime<Utc>>,
}

/// What one run found.
enum Sealed {
    /// Every full segment sealed; the layout as committed, and whether
    /// retention is due a look: a segment was sealed on a bounded topic.
    Done { layout: Layout, retain: bool },
    /// The partition is gone.
    Gone,
}

impl Sealer {
    /// The sealer of one instance.
    pub fn new(shared: Arc<Shared>, pool: PgPool) -> Self {
        Self { shared, pool }
    }

    // A segment is full once the head is at or past its last sequence: an
    // append commits its rows and its `hwm` together, and sequences have no
    // gaps, so every row of it is visible and none is still to come.
    async fn seal(&self, key: Key) -> Result<Sealed, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let (topic, partition) = key;
        let row: Option<PartitionRow> = sqlx::query_as(
            "SELECT p.hwm, l.max_age IS NOT NULL OR l.max_bytes IS NOT NULL AS bounded \
             FROM evbk__partition p JOIN evbk__log l USING (topic_uuid) \
             WHERE p.topic_uuid = $1 AND p.partition = $2",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(PartitionRow { hwm, bounded }) = row else {
            return Ok(Sealed::Gone);
        };
        // Another instance sealing a segment finishes it.
        let full: Vec<FullRow> = sqlx::query_as(
            "SELECT first_seq AS first, table_name AS segment FROM evbk__segment \
             WHERE topic_uuid = $1 AND partition = $2 AND state = 'open' AND end_seq - 1 <= $3 \
             ORDER BY first_seq FOR UPDATE SKIP LOCKED",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .bind(hwm)
        .fetch_all(&mut *tx)
        .await?;
        let sealed = !full.is_empty();
        for FullRow { first, segment } in full {
            // The segment's own table, not the parent: one table to plan.
            // Events deleted by hand (`docs/DESIGN.md` §4.10) leave a table
            // as large as what stays, and one with none left a `NULL` time.
            let measured: Measured = sqlx::query_as(AssertSqlSafe(format!(
                "SELECT pg_total_relation_size('{segment}'::regclass) AS bytes, \
                 (SELECT sequence_time FROM {segment} ORDER BY sequence DESC LIMIT 1) AS newest_at"
            )))
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE evbk__segment SET state = 'sealed', bytes = $4, newest_at = $5, \
                 changed_at = now() WHERE topic_uuid = $1 AND partition = $2 AND first_seq = $3",
            )
            .bind(topic)
            .bind(names::partition_column(partition))
            .bind(first)
            .bind(measured.bytes)
            .bind(measured.newest_at)
            .execute(&mut *tx)
            .await?;
        }
        let segments = catalog::live_segments(&mut tx, key).await?;
        tx.commit().await?;
        Ok(Sealed::Done {
            layout: Layout::new(&segments),
            retain: sealed && bounded,
        })
    }
}

impl WorkerAction for Sealer {
    type Payload = ();
    type Error = MediumDown;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, MediumDown> {
        let hub = &self.shared.maintenance;
        let lease = match hub.seal.take() {
            Taken::Lease(lease) => lease,
            Taken::Until(at) => return Ok(Directive::sleep(at - Instant::now())),
            Taken::Empty => return Ok(Directive::idle()),
        };
        // Stopping: the dropped lease puts the key back.
        if cancel.is_cancelled() {
            return Ok(Directive::idle());
        }
        let key = lease.key();
        match self.seal(key).await {
            Ok(Sealed::Done { layout, retain }) => {
                hub.registry.replace(key, layout);
                if retain {
                    hub.retain.offer(key, ());
                }
                lease.done();
            }
            Ok(Sealed::Gone) => {
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
        Ok(match hub.seal.peek() {
            Due::Now => Directive::proceed(),
            Due::At(at) => Directive::sleep(at - Instant::now()),
            Due::Never => Directive::idle(),
        })
    }
}
