//! Retention (`docs/DESIGN.md` §4.9): raises a partition's floor past the
//! oldest sealed segments its topic's bound no longer keeps, and marks them
//! expired for the detacher. No DDL: readers stop resolving into them at
//! this commit, since `resolve`, `read` and `query` read nothing at or below
//! the floor.

use std::sync::Arc;

use sqlx::{AssertSqlSafe, PgPool};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};

use super::classify::{self, Class, Loudness, MediumDown, lock_timeout_for};
use super::prioritizer::{Due, Key, Taken};
use crate::names;
use crate::shared::Shared;

/// Names the job in logs and worker names.
pub const NAME: &str = "retention";

/// Sealed segments one run looks at; a partition with more comes back at
/// once.
const BATCH: i64 = 64;

/// Expires a partition's oldest segments its bound no longer keeps, one
/// partition per pass.
pub struct Retention {
    shared: Arc<Shared>,
    pool: PgPool,
}

/// What one run found.
enum Retained {
    /// Nothing more is due.
    Done,
    /// Segments expired; `more` when the batch was full.
    Expired { more: bool },
    /// Another instance holds the partition's retention row, and finishes it.
    Busy,
    /// The partition is gone.
    Gone,
}

impl Retention {
    /// The retention of one instance.
    pub fn new(shared: Arc<Shared>, pool: PgPool) -> Self {
        Self { shared, pool }
    }

    async fn retain(&self, key: Key, attempt: u32) -> Result<Retained, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::raw_sql(AssertSqlSafe(format!(
            "SET LOCAL lock_timeout = '{}ms'",
            lock_timeout_for(attempt).as_millis()
        )))
        .execute(&mut *tx)
        .await?;
        let (topic, partition) = key;
        let row: Option<Bounds> = match sqlx::query_as(
            "SELECT l.max_age IS NOT NULL AS by_age, l.max_bytes \
             FROM evbk__retention r JOIN evbk__log l USING (topic_uuid) \
             WHERE r.topic_uuid = $1 AND r.partition = $2 FOR UPDATE OF r NOWAIT",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .fetch_optional(&mut *tx)
        .await
        {
            Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("55P03") => {
                return Ok(Retained::Busy);
            }
            row => row?,
        };
        let Some(Bounds { by_age, max_bytes }) = row else {
            return Ok(Retained::Gone);
        };
        // The early returns below end the transaction, and with it the row
        // lock, now rather than when the pool next pings the connection.
        if !by_age && max_bytes.is_none() {
            tx.commit().await?;
            return Ok(Retained::Done);
        }
        // What the partition takes on disk: each sealed segment as measured
        // when sealed, and each open one, the empty ones ahead of the head
        // included, as it is now. Only with a byte bound: the size reads
        // every open table's files.
        let mut stored: i64 = match max_bytes {
            Some(_) => {
                sqlx::query_scalar(
                    "SELECT COALESCE(SUM(CASE WHEN state = 'open' \
                     THEN COALESCE(pg_total_relation_size(to_regclass(table_name)), 0) ELSE bytes END), 0)::bigint \
                     FROM evbk__segment WHERE topic_uuid = $1 AND partition = $2 AND state IN ('open', 'sealed')",
                )
                .bind(topic)
                .bind(names::partition_column(partition))
                .fetch_one(&mut *tx)
                .await?
            }
            None => 0,
        };
        // Sealed with no event left (deleted by hand) is past any age.
        let sealed: Vec<SealedRow> = sqlx::query_as(
            "SELECT s.first_seq AS first, s.end_seq AS \"end\", s.bytes, \
             l.max_age IS NOT NULL AND (s.newest_at IS NULL OR s.newest_at < now() - l.max_age) AS aged \
             FROM evbk__segment s JOIN evbk__log l USING (topic_uuid) \
             WHERE s.topic_uuid = $1 AND s.partition = $2 AND s.state = 'sealed' \
             ORDER BY s.first_seq LIMIT $3",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .bind(BATCH)
        .fetch_all(&mut *tx)
        .await?;
        let full = i64::try_from(sealed.len()).is_ok_and(|n| n == BATCH);
        let mut due = Vec::new();
        let mut floor = 0;
        for SealedRow {
            first,
            end,
            bytes,
            aged,
        } in sealed
        {
            // A segment goes only while what stays still holds `max_bytes`.
            let over = max_bytes.is_some_and(|max| stored - bytes >= max);
            // Oldest first: what stays is a suffix.
            if !aged && !over {
                break;
            }
            due.push(first);
            floor = end - 1;
            stored -= bytes;
        }
        if due.is_empty() {
            tx.commit().await?;
            return Ok(Retained::Done);
        }
        sqlx::query(
            "UPDATE evbk__retention SET floor = $3 WHERE topic_uuid = $1 AND partition = $2",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .bind(floor)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE evbk__segment SET state = 'expired', changed_at = now() \
             WHERE topic_uuid = $1 AND partition = $2 AND first_seq = ANY($3)",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .bind(&due)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Retained::Expired {
            more: full && i64::try_from(due.len()).is_ok_and(|n| n == BATCH),
        })
    }
}

/// A partition's bounds, read under its retention row lock.
#[derive(sqlx::FromRow)]
struct Bounds {
    by_age: bool,
    max_bytes: Option<i64>,
}

/// One sealed segment, oldest first.
#[derive(sqlx::FromRow)]
struct SealedRow {
    first: i64,
    end: i64,
    /// Its size on disk when sealed.
    bytes: i64,
    /// Its newest event is older than `max_age`, or it has none.
    aged: bool,
}

impl WorkerAction for Retention {
    type Payload = ();
    type Error = MediumDown;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, MediumDown> {
        let hub = &self.shared.maintenance;
        let lease = match hub.retain.take() {
            Taken::Lease(lease) => lease,
            Taken::Until(at) => return Ok(Directive::sleep(at - Instant::now())),
            Taken::Empty => return Ok(Directive::idle()),
        };
        // Stopping: the dropped lease puts the key back.
        if cancel.is_cancelled() {
            return Ok(Directive::idle());
        }
        let key = lease.key();
        match self.retain(key, lease.attempt()).await {
            Ok(Retained::Done) => lease.done(),
            Ok(Retained::Expired { more }) => {
                hub.detach.offer(key, ());
                if more {
                    lease.again();
                } else {
                    lease.done();
                }
            }
            Ok(Retained::Busy) => lease.busy(),
            Ok(Retained::Gone) => {
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
        Ok(match hub.retain.peek() {
            Due::Now => Directive::proceed(),
            Due::At(at) => Directive::sleep(at - Instant::now()),
            Due::Never => Directive::idle(),
        })
    }
}
