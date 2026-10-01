//! The counter (`docs/DESIGN.md` §4.9): sums each open segment's
//! `stored_bytes` up to the head, and seals a segment once counted to its
//! end, so retention can weigh and age whole segments.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use sqlx::{AssertSqlSafe, PgPool};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};

use super::catalog::{self, SegmentState};
use super::classify::{self, Class, Loudness, MediumDown, statement_timeout_for};
use super::prioritizer::{Due, Key, Taken};
use super::registry::Layout;
use super::retention::Overdue;
use crate::names;
use crate::shared::Shared;

/// Names the job in logs and worker names.
pub const NAME: &str = "counter";

/// Counts the open segments of a partition, one partition per pass.
pub struct Counter {
    shared: Arc<Shared>,
    pool: PgPool,
}

/// What one run found.
enum Counted {
    /// Counted up to the head; the layout as committed, and how far over its
    /// bound the partition is when a segment was sealed on a bounded topic.
    Done(Layout, Option<Overdue>),
    /// The partition is gone.
    Gone,
}

impl Counter {
    /// The counter of one instance.
    pub fn new(shared: Arc<Shared>, pool: PgPool) -> Self {
        Self { shared, pool }
    }

    // Rows up to `hwm` are complete and counted once: `hwm` is read first, an
    // append commits its rows and its `hwm` together, and sequences have no
    // gaps, so once a `hwm` is visible every row at or below it is; and
    // `counted_to` moves only here, under the segment's row lock.
    async fn count(&self, key: Key, attempt: u32) -> Result<Counted, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::raw_sql(AssertSqlSafe(format!(
            "SET LOCAL statement_timeout = '{}ms'",
            statement_timeout_for(attempt).as_millis()
        )))
        .execute(&mut *tx)
        .await?;
        let (topic, partition) = key;
        let row: Option<(i64, String, bool, i64)> = sqlx::query_as(
            "SELECT p.hwm, p.table_name, l.max_age IS NOT NULL OR l.max_bytes IS NOT NULL, \
             GREATEST(COALESCE(p.appended_bytes - r.removed_bytes - l.max_bytes, 0), 0) \
             FROM evbk__partition p JOIN evbk__log l USING (topic_uuid) \
             JOIN evbk__retention r USING (topic_uuid, partition) \
             WHERE p.topic_uuid = $1 AND p.partition = $2",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .fetch_optional(&mut *tx)
        .await?;
        let Some((hwm, parent, bounded, overdue)) = row else {
            return Ok(Counted::Gone);
        };
        // Another instance counting a segment finishes it.
        let open: Vec<(i64, i64, i64)> = sqlx::query_as(
            "SELECT first_seq, end_seq, counted_to FROM evbk__segment \
             WHERE topic_uuid = $1 AND partition = $2 AND state = 'open' AND counted_to < $3 \
             ORDER BY first_seq FOR UPDATE SKIP LOCKED",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .bind(hwm)
        .fetch_all(&mut *tx)
        .await?;
        let mut sealed = false;
        for (first, end, counted_to) in open {
            let upto = hwm.min(end - 1);
            let (bytes, newest_at): (i64, Option<DateTime<Utc>>) =
                sqlx::query_as(AssertSqlSafe(format!(
                    "SELECT COALESCE(SUM(stored_bytes), 0)::bigint, MAX(sequence_time) FROM {parent} \
                     WHERE sequence > $1 AND sequence <= $2"
                )))
                .bind(counted_to)
                .bind(upto)
                .fetch_one(&mut *tx)
                .await?;
            let state: SegmentState = sqlx::query_scalar(
                "UPDATE evbk__segment SET bytes = bytes + $4, counted_to = $5, \
                 newest_at = COALESCE($6, newest_at), changed_at = now(), \
                 state = CASE WHEN $5 = end_seq - 1 THEN 'sealed'::evbk__segment_state ELSE state END \
                 WHERE topic_uuid = $1 AND partition = $2 AND first_seq = $3 RETURNING state",
            )
            .bind(topic)
            .bind(names::partition_column(partition))
            .bind(first)
            .bind(bytes)
            .bind(upto)
            .bind(newest_at)
            .fetch_one(&mut *tx)
            .await?;
            sealed |= state == SegmentState::Sealed;
        }
        let segments = catalog::live_segments(&mut tx, key).await?;
        tx.commit().await?;
        let retain = (sealed && bounded).then_some(Overdue(overdue));
        Ok(Counted::Done(Layout::new(&segments, bounded), retain))
    }
}

impl WorkerAction for Counter {
    type Payload = ();
    type Error = MediumDown;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, MediumDown> {
        let hub = &self.shared.maintenance;
        let lease = match hub.count.take() {
            Taken::Lease(lease) => lease,
            Taken::Until(at) => return Ok(Directive::sleep(at - Instant::now())),
            Taken::Empty => return Ok(Directive::idle()),
        };
        // Stopping: the dropped lease puts the key back.
        if cancel.is_cancelled() {
            return Ok(Directive::idle());
        }
        let key = lease.key();
        match self.count(key, lease.attempt()).await {
            Ok(Counted::Done(layout, retain)) => {
                hub.registry.replace(key, layout);
                if let Some(overdue) = retain {
                    hub.retain.offer(key, overdue);
                }
                lease.done();
            }
            Ok(Counted::Gone) => {
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
        Ok(match hub.count.peek() {
            Due::Now => Directive::proceed(),
            Due::At(at) => Directive::sleep(at - Instant::now()),
            Due::Never => Directive::idle(),
        })
    }
}
