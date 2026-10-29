//! One maintenance run over one partition (`docs/DESIGN.md` §4.9), under the
//! partition's named lock, in this order:
//!
//! 1. settle: catalog rows whose DDL a crash cut off, and range partitions
//!    with no row, from `information_schema.PARTITIONS`;
//! 2. extend: the event table created when missing, and range partitions
//!    added until two lie wholly above the head;
//! 3. seal: each full open segment weighed and timed;
//! 4. retain: the floor raised past the oldest sealed segments the topic's
//!    bounds no longer keep, which are marked expired, in one transaction;
//! 5. drop: the expired range partitions dropped, then their rows deleted.
//!
//! A DDL commits on its own, so each is bracketed by catalog writes - the
//! rows `adding` before it and `open` after, `expired` before a drop and
//! deleted after - and step 1 of a later run settles any row a crash left
//! between the two.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::{AssertSqlSafe, Connection, MySqlConnection, MySqlPool};

use super::catalog::{self, Held, Locked, Segment, SegmentState};
use super::hub::Layout;
use crate::heads::Key;
use crate::names;
use crate::segment::{self, Observed};
use crate::shared::Shared;

/// What one run found.
#[derive(Debug)]
pub enum Outcome {
    /// Every step done; the layout as left.
    Done(Layout),
    /// Another session holds the partition's lock, and does the work.
    Busy,
    /// The partition has no row.
    Gone,
}

/// What a run reads of its partition first.
#[derive(sqlx::FromRow)]
struct PartitionRow {
    table_name: String,
    hwm: i64,
    bytes: i64,
    floor_seq: i64,
    removed_bytes: i64,
    max_age_ms: Option<i64>,
    max_bytes: Option<i64>,
    now: DateTime<Utc>,
}

/// One run over `key` on a connection of `pool`, waiting up to `lock_wait`
/// seconds for the partition's lock; the `attempt`-th in a row for this
/// partition, which lengthens the DDL's metadata lock wait.
pub async fn run(
    shared: &Shared,
    pool: &MySqlPool,
    key: Key,
    attempt: u32,
    lock_wait: u64,
) -> Result<Outcome, sqlx::Error> {
    let (topic, partition) = key;
    let lock = names::segment_lock(&shared.settings.database, topic, partition);
    let conn = pool.acquire().await?;
    let Some(mut locked) = Locked::acquire(conn, lock, lock_wait).await? else {
        return Ok(Outcome::Busy);
    };
    let wait = shared
        .settings
        .maintenance
        .ddl_lock_wait
        .as_secs()
        .saturating_mul(u64::from(attempt.min(3) + 1));
    sqlx::query(AssertSqlSafe(format!(
        "SET SESSION lock_wait_timeout = {wait}"
    )))
    .execute(locked.conn())
    .await?;
    let outcome = steps(shared, locked.conn(), key).await?;
    sqlx::query("SET SESSION lock_wait_timeout = DEFAULT")
        .execute(locked.conn())
        .await?;
    locked.release().await?;
    Ok(outcome)
}

async fn steps(
    shared: &Shared,
    conn: &mut MySqlConnection,
    key: Key,
) -> Result<Outcome, sqlx::Error> {
    let (topic, partition) = key;
    let row: Option<PartitionRow> = sqlx::query_as(
        "SELECT p.table_name, p.hwm, p.bytes, r.floor_seq, r.removed_bytes, l.max_age_ms, \
         l.max_bytes, UTC_TIMESTAMP(6) AS now \
         FROM evbk__partition p \
         JOIN evbk__retention r ON r.topic_uuid = p.topic_uuid AND r.partition_no = p.partition_no \
         JOIN evbk__log l ON l.topic_uuid = p.topic_uuid \
         WHERE p.topic_uuid = ? AND p.partition_no = ?",
    )
    .bind(topic)
    .bind(names::partition_column(partition))
    .fetch_optional(&mut *conn)
    .await?;
    let Some(row) = row else {
        return Ok(Outcome::Gone);
    };
    let table = row.table_name.as_str();

    let held = catalog::held(conn, table).await?;
    settle(conn, key, table, held.as_deref(), row.floor_seq).await?;
    let mut segments = catalog::segments(conn, key).await?;

    extend(
        shared,
        conn,
        key,
        table,
        held.is_some(),
        row.hwm,
        &mut segments,
    )
    .await?;
    seal(conn, key, table, row.hwm, row.removed_bytes, &mut segments).await?;
    retain(conn, key, &row, &mut segments).await?;
    drop_expired(conn, key, table, &mut segments).await?;
    Ok(Outcome::Done(Layout::new(&segments)))
}

/// Step 1: every row against what the server holds of `table`.
async fn settle(
    conn: &mut MySqlConnection,
    key: Key,
    table: &str,
    held: Option<&[Held]>,
    floor: i64,
) -> Result<(), sqlx::Error> {
    let rows = catalog::segments(conn, key).await?;
    let held_names: HashSet<&str> = held
        .unwrap_or_default()
        .iter()
        .map(|h| h.name.as_str())
        .collect();
    for row in &rows {
        let exists = held_names.contains(row.name.as_str());
        match row.state {
            // Cut off around its ADD PARTITION or CREATE TABLE.
            SegmentState::Adding if exists => set_open(conn, key, &[row.first]).await?,
            // Cut off around its DROP PARTITION.
            SegmentState::Adding | SegmentState::Expired if !exists => {
                delete_row(conn, key, row.first).await?;
            }
            SegmentState::Open | SegmentState::Sealed if !exists => {
                let (topic, partition) = key;
                tracing::error!(%topic, %partition, segment = %row.name,
                    "event-broker maintenance: a segment row names a range partition the server does not hold; its events are lost");
            }
            _ => {}
        }
    }
    match held {
        Some(held) => adopt(conn, key, table, held, &rows, floor).await,
        None => Ok(()),
    }
}

/// Range partitions with no row: added by a run cut off before it wrote its
/// rows, or made by hand. Below the floor they are leftovers and dropped;
/// above, they are adopted as open.
async fn adopt(
    conn: &mut MySqlConnection,
    key: Key,
    table: &str,
    held: &[Held],
    rows: &[Segment],
    floor: i64,
) -> Result<(), sqlx::Error> {
    let recorded: HashSet<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    let mut leftovers = Vec::new();
    for (i, partition) in held.iter().enumerate() {
        if recorded.contains(partition.name.as_str()) {
            continue;
        }
        let first = if i == 0 { 1 } else { held[i - 1].end };
        if partition.end - 1 <= floor {
            leftovers.push(partition.name.clone());
        } else {
            insert_rows(
                conn,
                key,
                &[(first, partition.end, partition.name.clone())],
                "open",
            )
            .await?;
        }
    }
    if !leftovers.is_empty() && leftovers.len() < held.len() {
        sqlx::query(AssertSqlSafe(format!(
            "ALTER TABLE {table} DROP PARTITION {}",
            leftovers.join(", ")
        )))
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Step 2: the table when the server holds none, else the range partitions
/// the head needs.
async fn extend(
    shared: &Shared,
    conn: &mut MySqlConnection,
    key: Key,
    table: &str,
    table_exists: bool,
    hwm: i64,
    segments: &mut Vec<Segment>,
) -> Result<(), sqlx::Error> {
    let live: Vec<(i64, i64)> = segments
        .iter()
        .filter(|s| s.is_live())
        .map(|s| (s.first, s.end))
        .collect();
    if !table_exists && !live.is_empty() {
        // Reported by `settle`: making the table again would hide the loss.
        return Ok(());
    }
    let span = segment::span(shared.settings.segment, observed(segments));
    let ranges = segment::ahead(hwm, &live, span);
    if ranges.is_empty() {
        return Ok(());
    }
    let added: Vec<Segment> = ranges
        .iter()
        .map(|&(first, end)| Segment::adding(first, end))
        .collect();
    let rows: Vec<(i64, i64, String)> = added
        .iter()
        .map(|s| (s.first, s.end, s.name.clone()))
        .collect();
    insert_rows(conn, key, &rows, "adding").await?;
    let partitions = added
        .iter()
        .map(|s| format!("PARTITION {} VALUES LESS THAN ({})", s.name, s.end))
        .collect::<Vec<_>>()
        .join(", ");
    let ddl = if table_exists {
        format!("ALTER TABLE {table} ADD PARTITION ({partitions})")
    } else {
        format!(
            "CREATE TABLE IF NOT EXISTS {table} (\
                 seq BIGINT NOT NULL, \
                 seq_time DATETIME(6) NOT NULL, \
                 tenant_id BINARY(16) NOT NULL, \
                 cum_bytes BIGINT NOT NULL, \
                 body LONGBLOB NOT NULL, \
                 PRIMARY KEY (seq)\
             ) ENGINE = InnoDB PARTITION BY RANGE (seq) ({partitions})"
        )
    };
    sqlx::query(AssertSqlSafe(ddl)).execute(&mut *conn).await?;
    let firsts: Vec<i64> = added.iter().map(|s| s.first).collect();
    set_open(conn, key, &firsts).await?;
    segments.retain(|s| s.state != SegmentState::Adding);
    segments.extend(added.into_iter().map(|s| Segment {
        state: SegmentState::Open,
        ..s
    }));
    segments.sort_by_key(|s| s.first);
    Ok(())
}

/// Step 3: each open segment the head has passed the end of, weighed from
/// the running weight its events carry. Sequences have no gaps and an append
/// commits its rows with the head, so every row of a full segment is
/// visible.
async fn seal(
    conn: &mut MySqlConnection,
    (topic, partition): Key,
    table: &str,
    hwm: i64,
    removed_bytes: i64,
    segments: &mut [Segment],
) -> Result<(), sqlx::Error> {
    // The weight of everything below the segment being sealed: the expired
    // and dropped segments' in `removed_bytes`, the sealed ones' in their
    // rows.
    let mut below = removed_bytes;
    for segment in segments.iter_mut() {
        match segment.state {
            SegmentState::Sealed => below += segment.bytes,
            SegmentState::Open if segment.end - 1 <= hwm => {
                let last: Option<(i64, DateTime<Utc>)> = sqlx::query_as(AssertSqlSafe(format!(
                    "SELECT cum_bytes, seq_time FROM {table} WHERE seq >= ? AND seq < ? \
                     ORDER BY seq DESC LIMIT 1"
                )))
                .bind(segment.first)
                .bind(segment.end)
                .fetch_optional(&mut *conn)
                .await?;
                let (bytes, newest_at) = match last {
                    Some((cumulative, at)) => ((cumulative - below).max(0), Some(at)),
                    None => (0, None),
                };
                sqlx::query(
                    "UPDATE evbk__segment SET state = 'sealed', bytes = ?, newest_at = ?, \
                     changed_at = UTC_TIMESTAMP(6) \
                     WHERE topic_uuid = ? AND partition_no = ? AND first_seq = ?",
                )
                .bind(bytes)
                .bind(newest_at)
                .bind(topic)
                .bind(names::partition_column(partition))
                .bind(segment.first)
                .execute(&mut *conn)
                .await?;
                segment.state = SegmentState::Sealed;
                segment.bytes = bytes;
                segment.newest_at = newest_at;
                below += bytes;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Step 4: the oldest sealed segments past a bound, expired with the floor
/// in one transaction; readers stop resolving into them at its commit.
async fn retain(
    conn: &mut MySqlConnection,
    (topic, partition): Key,
    row: &PartitionRow,
    segments: &mut [Segment],
) -> Result<(), sqlx::Error> {
    if row.max_age_ms.is_none() && row.max_bytes.is_none() {
        return Ok(());
    }
    let aged_before = row
        .max_age_ms
        .map(|ms| row.now - chrono::Duration::milliseconds(ms));
    let mut stored = row.bytes - row.removed_bytes;
    let mut floor = row.floor_seq;
    let mut removed = 0;
    let mut due = Vec::new();
    for segment in segments.iter().filter(|s| s.state == SegmentState::Sealed) {
        // Sealed with no event left (deleted by hand) is past any age.
        let aged = aged_before.is_some_and(|before| segment.newest_at.is_none_or(|n| n < before));
        // A segment goes only while what stays still holds `max_bytes`.
        let over = row
            .max_bytes
            .is_some_and(|max| stored - segment.bytes >= max);
        // Oldest first: what stays is a suffix.
        if !aged && !over {
            break;
        }
        due.push(segment.first);
        floor = floor.max(segment.end - 1);
        removed += segment.bytes;
        stored -= segment.bytes;
    }
    if due.is_empty() {
        return Ok(());
    }
    let mut tx = conn.begin().await?;
    sqlx::query(
        "UPDATE evbk__retention SET floor_seq = ?, removed_bytes = removed_bytes + ? \
         WHERE topic_uuid = ? AND partition_no = ?",
    )
    .bind(floor)
    .bind(removed)
    .bind(topic)
    .bind(names::partition_column(partition))
    .execute(&mut *tx)
    .await?;
    let placeholders = vec!["?"; due.len()].join(", ");
    let mut expire = sqlx::query(AssertSqlSafe(format!(
        "UPDATE evbk__segment SET state = 'expired', changed_at = UTC_TIMESTAMP(6) \
         WHERE topic_uuid = ? AND partition_no = ? AND first_seq IN ({placeholders})"
    )))
    .bind(topic)
    .bind(names::partition_column(partition));
    for first in &due {
        expire = expire.bind(*first);
    }
    expire.execute(&mut *tx).await?;
    tx.commit().await?;
    for segment in segments.iter_mut().filter(|s| due.contains(&s.first)) {
        segment.state = SegmentState::Expired;
    }
    Ok(())
}

/// Step 5: the expired range partitions dropped in one DDL, then their rows.
async fn drop_expired(
    conn: &mut MySqlConnection,
    key: Key,
    table: &str,
    segments: &mut Vec<Segment>,
) -> Result<(), sqlx::Error> {
    let expired: Vec<&Segment> = segments
        .iter()
        .filter(|s| s.state == SegmentState::Expired)
        .collect();
    // A table keeps at least one range partition: the server refuses to
    // drop them all, and the extender keeps live ones above any expired.
    if expired.is_empty() || expired.len() == segments.len() {
        return Ok(());
    }
    let names: Vec<&str> = expired.iter().map(|s| s.name.as_str()).collect();
    sqlx::query(AssertSqlSafe(format!(
        "ALTER TABLE {table} DROP PARTITION {}",
        names.join(", ")
    )))
    .execute(&mut *conn)
    .await?;
    let firsts: Vec<i64> = expired.iter().map(|s| s.first).collect();
    for first in &firsts {
        delete_row(conn, key, *first).await?;
    }
    segments.retain(|s| s.state != SegmentState::Expired);
    Ok(())
}

async fn insert_rows(
    conn: &mut MySqlConnection,
    (topic, partition): Key,
    rows: &[(i64, i64, String)],
    state: &'static str,
) -> Result<(), sqlx::Error> {
    let values = vec!["(?, ?, ?, ?, ?, ?, UTC_TIMESTAMP(6))"; rows.len()].join(", ");
    let mut insert = sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO evbk__segment \
         (topic_uuid, partition_no, first_seq, end_seq, segment_name, state, changed_at) VALUES {values}"
    )));
    for (first, end, name) in rows {
        insert = insert
            .bind(topic)
            .bind(names::partition_column(partition))
            .bind(*first)
            .bind(*end)
            .bind(name.as_str())
            .bind(state);
    }
    insert.execute(conn).await?;
    Ok(())
}

async fn set_open(
    conn: &mut MySqlConnection,
    (topic, partition): Key,
    firsts: &[i64],
) -> Result<(), sqlx::Error> {
    let placeholders = vec!["?"; firsts.len()].join(", ");
    let mut update = sqlx::query(AssertSqlSafe(format!(
        "UPDATE evbk__segment SET state = 'open', changed_at = UTC_TIMESTAMP(6) \
         WHERE topic_uuid = ? AND partition_no = ? AND first_seq IN ({placeholders})"
    )))
    .bind(topic)
    .bind(names::partition_column(partition));
    for first in firsts {
        update = update.bind(*first);
    }
    update.execute(conn).await?;
    Ok(())
}

async fn delete_row(
    conn: &mut MySqlConnection,
    (topic, partition): Key,
    first: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM evbk__segment WHERE topic_uuid = ? AND partition_no = ? AND first_seq = ?",
    )
    .bind(topic)
    .bind(names::partition_column(partition))
    .bind(first)
    .execute(conn)
    .await?;
    Ok(())
}

/// The rate between the ends of the two newest sealed segments of a
/// partition, for a `period` rule.
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
