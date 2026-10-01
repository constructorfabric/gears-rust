//! The queries over `evbk__segment` the jobs share, and the advisory lock
//! names that keep instances apart.

use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use super::prioritizer::Key;
use crate::names;

/// Where a segment is in its life (`docs/DESIGN.md` §4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "evbk__segment_state", rename_all = "snake_case")]
pub enum SegmentState {
    /// Attached, and not yet counted to its end.
    Open,
    /// Attached, and counted to its end.
    Sealed,
    /// Below the floor: no reader resolves into it; still attached.
    Expired,
    /// Being detached, and then dropped.
    Detaching,
}

/// One `evbk__segment` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// The first sequence of the segment.
    pub first: i64,
    /// One past its last sequence.
    pub end: i64,
    /// Where it is in its life.
    pub state: SegmentState,
    /// The last sequence its size counts; `first - 1` before any.
    pub counted_to: i64,
    /// The `sequence_time` at `counted_to`.
    pub newest_at: Option<DateTime<Utc>>,
}

impl Segment {
    /// A segment just created: open and not counted.
    pub fn open(first: i64, end: i64) -> Self {
        Self {
            first,
            end,
            state: SegmentState::Open,
            counted_to: first - 1,
            newest_at: None,
        }
    }

    /// Whether the segment is attached and readers may resolve into it.
    pub fn is_live(&self) -> bool {
        matches!(self.state, SegmentState::Open | SegmentState::Sealed)
    }
}

type SegmentRow = (i64, i64, SegmentState, i64, Option<DateTime<Utc>>);
type KeyedSegmentRow = (
    Uuid,
    i32,
    i64,
    i64,
    SegmentState,
    i64,
    Option<DateTime<Utc>>,
);

fn segment((first, end, state, counted_to, newest_at): SegmentRow) -> Segment {
    Segment {
        first,
        end,
        state,
        counted_to,
        newest_at,
    }
}

/// The open and sealed segments of `key`, oldest first.
pub async fn live_segments(
    conn: &mut PgConnection,
    (topic, partition): Key,
) -> Result<Vec<Segment>, sqlx::Error> {
    let rows: Vec<SegmentRow> = sqlx::query_as(
        "SELECT first_seq, end_seq, state, counted_to, newest_at FROM evbk__segment \
         WHERE topic_uuid = $1 AND partition = $2 AND state IN ('open', 'sealed') ORDER BY first_seq",
    )
    .bind(topic)
    .bind(names::partition_column(partition))
    .fetch_all(conn)
    .await?;
    Ok(rows.into_iter().map(segment).collect())
}

/// Every segment row, by partition, oldest first.
pub async fn all_segments(conn: &mut PgConnection) -> Result<Vec<(Key, Segment)>, sqlx::Error> {
    let rows: Vec<KeyedSegmentRow> = sqlx::query_as(
        "SELECT topic_uuid, partition, first_seq, end_seq, state, counted_to, newest_at \
             FROM evbk__segment ORDER BY topic_uuid, partition, first_seq",
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(
            |(topic, partition, first, end, state, counted_to, newest_at)| {
                let partition = names::partition_from_column(partition)?;
                Some((
                    (topic, partition),
                    segment((first, end, state, counted_to, newest_at)),
                ))
            },
        )
        .collect())
}

/// Records the segment `table` just attached, open and not counted.
pub async fn insert_open(
    conn: &mut PgConnection,
    (topic, partition): Key,
    (first, end): (i64, i64),
    table: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO evbk__segment (topic_uuid, partition, first_seq, end_seq, table_name, state, counted_to, changed_at) \
         VALUES ($1, $2, $3, $4, $5, 'open', $3 - 1, now())",
    )
    .bind(topic)
    .bind(names::partition_column(partition))
    .bind(first)
    .bind(end)
    .bind(table)
    .execute(conn)
    .await?;
    Ok(())
}

/// The advisory lock under which segments of `key` are created or repaired.
pub fn segment_lock((topic, partition): Key) -> String {
    format!("evbk:seg:{topic}:{partition}")
}

/// The advisory lock under which segments of `key` are detached or
/// repaired: the detacher holds it per session, reconcile per transaction,
/// and the two conflict.
pub fn detach_lock((topic, partition): Key) -> String {
    format!("evbk:det:{topic}:{partition}")
}

/// Takes `lock` until the transaction ends; `false` when another session
/// holds it.
pub async fn try_xact_lock(conn: &mut PgConnection, lock: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock)
        .fetch_one(conn)
        .await
}
