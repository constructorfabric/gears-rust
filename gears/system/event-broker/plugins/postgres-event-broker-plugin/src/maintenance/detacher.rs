//! The detacher (`docs/DESIGN.md` §4.9): detaches and drops a partition's
//! expired segments, one step per pass.
//!
//! The one job that is not one transaction: `DETACH ... CONCURRENTLY`
//! refuses a transaction block, so it runs on a connection of its own,
//! outside the pool. The segment's row records each step, so a run cut off
//! anywhere resumes from the row and the catalog.

use std::sync::Arc;
use std::time::Duration;

use sqlx::{AssertSqlSafe, Connection, PgConnection};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use toolkit_taskward::{Directive, WorkerAction};

use super::catalog::{self, SegmentState};
use super::classify::{self, Class, Loudness, MediumDown};
use super::prioritizer::{Due, Key, Taken};
use crate::names;
use crate::settings::PostgresSettings;
use crate::shared::Shared;
use crate::startup;

/// Names the job in logs and worker names.
pub const NAME: &str = "detacher";

/// The longest one detach step waits; below the extender's longest
/// `lock_timeout`, so an ATTACH queued behind it gets the lock in time.
const DETACH_WAIT: Duration = Duration::from_secs(2);

/// Detaches and drops expired segments, one step per pass.
pub struct Detacher {
    shared: Arc<Shared>,
    /// Opened on the first pass, and dropped after any error: a session
    /// that failed mid-step may still hold the partition's lock.
    conn: Option<PgConnection>,
}

/// What one step did.
enum Detached {
    /// No expired segment is left.
    Done,
    /// One step taken; more may follow.
    Again,
    /// Another instance is detaching this partition, and finishes it.
    Busy,
}

impl Detacher {
    /// The detacher of one instance; it connects on its first pass.
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared, conn: None }
    }

    async fn detach(&mut self, key: Key) -> Result<Detached, sqlx::Error> {
        let mut conn = match self.conn.take() {
            Some(conn) => conn,
            None => connect(&self.shared.settings).await?,
        };
        let detached = step(&mut conn, key).await?;
        self.conn = Some(conn);
        Ok(detached)
    }
}

async fn connect(settings: &PostgresSettings) -> Result<PgConnection, sqlx::Error> {
    let options = startup::connect_options(settings, "evbk-detacher")?;
    let mut conn = PgConnection::connect_with(&options).await?;
    // `DETACH ... CONCURRENTLY` waits out every transaction that uses the
    // parent while holding the parent's SHARE UPDATE EXCLUSIVE lock, which
    // the extender's ATTACH needs too. The statement timeout bounds that
    // wait: a detach cut off by it is left pending, holds no lock, and the
    // next pass finishes it with FINALIZE. The connection check ends a
    // detach whose client is gone, rather than at the timeout.
    sqlx::raw_sql(AssertSqlSafe(format!(
        "SET synchronous_commit = on; SET search_path TO {}; SET lock_timeout = 0; \
         SET statement_timeout = '{}ms'; SET client_connection_check_interval = '1s'; \
         SET idle_in_transaction_session_timeout = '30s'",
        settings.schema,
        DETACH_WAIT.as_millis()
    )))
    .execute(&mut conn)
    .await?;
    Ok(conn)
}

/// One step on the oldest expired or detaching segment of `key`, under the
/// partition's session lock. The lock is released on every path: unlocked
/// here on success, and on any error, a panic or a cancelled pass the
/// connection is dropped, which ends the session and every lock it holds.
async fn step(conn: &mut PgConnection, key: Key) -> Result<Detached, sqlx::Error> {
    let (topic, partition) = key;
    let lock = catalog::detach_lock(key);
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
        .bind(&lock)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        return Ok(Detached::Busy);
    }
    // Read under the lock, so another instance's steps are all behind it.
    let row: Option<NextRow> = sqlx::query_as(
        "SELECT s.first_seq AS first, s.state, s.table_name AS segment, p.table_name AS parent \
         FROM evbk__segment s JOIN evbk__partition p USING (topic_uuid, partition) \
         WHERE s.topic_uuid = $1 AND s.partition = $2 AND s.state IN ('expired', 'detaching') \
         ORDER BY s.first_seq LIMIT 1",
    )
    .bind(topic)
    .bind(names::partition_column(partition))
    .fetch_optional(&mut *conn)
    .await?;
    let detached = match row {
        None => Detached::Done,
        Some(NextRow {
            first,
            state,
            segment,
            parent,
        }) => {
            advance(conn, key, first, state, &segment, &parent).await?;
            Detached::Again
        }
    };
    sqlx::query("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
        .bind(&lock)
        .execute(&mut *conn)
        .await?;
    Ok(detached)
}

/// The oldest segment of a partition left to detach or drop.
#[derive(sqlx::FromRow)]
struct NextRow {
    first: i64,
    state: SegmentState,
    /// The segment's table.
    segment: String,
    /// The partitioned table.
    parent: String,
}

/// Takes the next step on `segment` of `parent`, from its row's `state` and
/// the catalog.
async fn advance(
    conn: &mut PgConnection,
    (topic, partition): Key,
    first: i64,
    state: SegmentState,
    segment: &str,
    parent: &str,
) -> Result<(), sqlx::Error> {
    // `None`: not attached, whether detached or already dropped.
    let pending: Option<bool> = sqlx::query_scalar(
        "SELECT i.inhdetachpending FROM pg_inherits i WHERE i.inhrelid = to_regclass($1)",
    )
    .bind(segment)
    .fetch_optional(&mut *conn)
    .await?;
    match (state, pending) {
        (SegmentState::Expired, Some(false)) => {
            sqlx::query(
                "UPDATE evbk__segment SET state = 'detaching', changed_at = now() \
                 WHERE topic_uuid = $1 AND partition = $2 AND first_seq = $3",
            )
            .bind(topic)
            .bind(names::partition_column(partition))
            .bind(first)
            .execute(&mut *conn)
            .await?;
            detach_concurrently(conn, parent, segment).await?;
        }
        // Cut off before the detach began.
        (_, Some(false)) => detach_concurrently(conn, parent, segment).await?,
        // Cut off after its first transaction: the partition is detached
        // for new queries and waits on old ones.
        (_, Some(true)) => {
            sqlx::raw_sql(AssertSqlSafe(format!(
                "ALTER TABLE {parent} DETACH PARTITION {segment} FINALIZE"
            )))
            .execute(&mut *conn)
            .await?;
        }
        (_, None) => {
            let mut tx = conn.begin().await?;
            sqlx::raw_sql(AssertSqlSafe(format!("DROP TABLE IF EXISTS {segment}")))
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "DELETE FROM evbk__segment WHERE topic_uuid = $1 AND partition = $2 AND first_seq = $3",
            )
            .bind(topic)
            .bind(names::partition_column(partition))
            .bind(first)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
    }
    Ok(())
}

/// Runs alone, outside any transaction block, as `CONCURRENTLY` requires.
async fn detach_concurrently(
    conn: &mut PgConnection,
    parent: &str,
    segment: &str,
) -> Result<(), sqlx::Error> {
    sqlx::raw_sql(AssertSqlSafe(format!(
        "ALTER TABLE {parent} DETACH PARTITION {segment} CONCURRENTLY"
    )))
    .execute(conn)
    .await?;
    Ok(())
}

impl WorkerAction for Detacher {
    type Payload = ();
    type Error = MediumDown;

    async fn execute(&mut self, cancel: &CancellationToken) -> Result<Directive, MediumDown> {
        let shared = Arc::clone(&self.shared);
        let hub = &shared.maintenance;
        let lease = match hub.detach.take() {
            Taken::Lease(lease) => lease,
            Taken::Until(at) => return Ok(Directive::sleep(at - Instant::now())),
            Taken::Empty => return Ok(Directive::idle()),
        };
        // Stopping: the dropped lease puts the key back.
        if cancel.is_cancelled() {
            return Ok(Directive::idle());
        }
        let key = lease.key();
        match self.detach(key).await {
            Ok(Detached::Done) => lease.done(),
            Ok(Detached::Again) => lease.again(),
            Ok(Detached::Busy) => lease.busy(),
            Err(err) => match classify::class(&err, lease.attempt()) {
                Class::Medium => {
                    lease.deferred();
                    return Err(MediumDown::new(NAME, &err));
                }
                Class::Gone => lease.gone(),
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
        Ok(match hub.detach.peek() {
            Due::Now => Directive::proceed(),
            Due::At(at) => Directive::sleep(at - Instant::now()),
            Due::Never => Directive::idle(),
        })
    }
}
