//! The catalog rows of a partition's segments, the server's own view of its
//! partitions, and the named lock that keeps instances apart.

use chrono::{DateTime, Utc};
use sqlx::MySql;
use sqlx::MySqlConnection;
use sqlx::pool::PoolConnection;

use crate::heads::Key;
use crate::{names, segment};

/// Where a segment is in its life (`docs/DESIGN.md` §4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentState {
    /// Its range partition is being added; the server may or may not hold
    /// it.
    Adding,
    /// Its range partition exists, and is not yet full.
    Open,
    /// Full, and weighed.
    Sealed,
    /// At or below the floor: no reader resolves into it; its range
    /// partition is dropped next, and then the row.
    Expired,
}

impl SegmentState {
    /// The state a `state` column value names; `None` for one this plugin
    /// did not write.
    pub fn parse(state: &str) -> Option<Self> {
        match state {
            "adding" => Some(Self::Adding),
            "open" => Some(Self::Open),
            "sealed" => Some(Self::Sealed),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

/// One `evbk__segment` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// The first sequence of the segment.
    pub first: i64,
    /// One past its last sequence: its partition's `VALUES LESS THAN`.
    pub end: i64,
    /// Its range partition.
    pub name: String,
    /// Where the segment is in its lifecycle.
    pub state: SegmentState,
    /// The weight of its events when sealed; 0 until then.
    pub bytes: i64,
    /// Its last event's `seq_time` when sealed; `None` until then, or with
    /// no event left.
    pub newest_at: Option<DateTime<Utc>>,
}

impl Segment {
    /// A segment being added.
    pub fn adding(first: i64, end: i64) -> Self {
        Self {
            first,
            end,
            name: names::segment(first),
            state: SegmentState::Adding,
            bytes: 0,
            newest_at: None,
        }
    }

    /// Whether the segment holds, or is to hold, readable events.
    pub fn is_live(&self) -> bool {
        matches!(self.state, SegmentState::Open | SegmentState::Sealed)
    }
}

type SegmentRow = (i64, i64, String, String, i64, Option<DateTime<Utc>>);

fn segment_of(
    (first, end, name, state, bytes, newest_at): SegmentRow,
) -> Result<Segment, sqlx::Error> {
    let state = SegmentState::parse(&state)
        .ok_or_else(|| sqlx::Error::Decode("evbk__segment holds an unknown state".into()))?;
    Ok(Segment {
        first,
        end,
        name,
        state,
        bytes,
        newest_at,
    })
}

/// Every segment row of `key`, oldest first.
pub async fn segments(
    conn: &mut MySqlConnection,
    (topic, partition): Key,
) -> Result<Vec<Segment>, sqlx::Error> {
    let rows: Vec<SegmentRow> = sqlx::query_as(
        "SELECT first_seq, end_seq, segment_name, CAST(state AS CHAR), bytes, newest_at \
         FROM evbk__segment WHERE topic_uuid = ? AND partition_no = ? ORDER BY first_seq",
    )
    .bind(topic)
    .bind(names::partition_column(partition))
    .fetch_all(conn)
    .await?;
    rows.into_iter().map(segment_of).collect()
}

/// A [`SegmentRow`] after its partition's `topic_uuid` and `partition_no`.
type KeyedSegmentRow = (
    uuid::Uuid,
    i32,
    i64,
    i64,
    String,
    String,
    i64,
    Option<DateTime<Utc>>,
);

/// Every segment row, by partition, oldest first.
pub async fn all_segments(conn: &mut MySqlConnection) -> Result<Vec<(Key, Segment)>, sqlx::Error> {
    let rows: Vec<KeyedSegmentRow> = sqlx::query_as(
        "SELECT topic_uuid, partition_no, first_seq, end_seq, segment_name, CAST(state AS CHAR), \
             bytes, newest_at FROM evbk__segment ORDER BY topic_uuid, partition_no, first_seq",
    )
    .fetch_all(conn)
    .await?;
    let mut segments = Vec::with_capacity(rows.len());
    for (topic, partition, first, end, name, state, bytes, newest_at) in rows {
        let Some(partition) = names::partition_from_column(partition) else {
            continue;
        };
        segments.push((
            (topic, partition),
            segment_of((first, end, name, state, bytes, newest_at))?,
        ));
    }
    Ok(segments)
}

/// One range partition the server holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    /// The range partition's name.
    pub name: String,
    /// Its `VALUES LESS THAN`.
    pub end: i64,
}

/// What the server holds of `table`: `None` when there is no such table,
/// else its range partitions, lowest first.
pub async fn held(
    conn: &mut MySqlConnection,
    table: &str,
) -> Result<Option<Vec<Held>>, sqlx::Error> {
    let rows: Vec<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT PARTITION_NAME, PARTITION_DESCRIPTION FROM information_schema.PARTITIONS \
         WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? ORDER BY PARTITION_ORDINAL_POSITION",
    )
    .bind(table)
    .fetch_all(conn)
    .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        rows.into_iter()
            .filter_map(|(name, bound)| {
                Some(Held {
                    name: name?,
                    end: segment::parse_bound(&bound?)?,
                })
            })
            .collect(),
    ))
}

/// A pooled connection holding a partition's named lock.
///
/// The lock belongs to the session, so it must never go back to the pool
/// held: [`Locked::release`] frees it and hands the connection back, and a
/// guard dropped without it, by an error or a cancelled run, closes the
/// connection, which ends the session and every lock it holds.
pub struct Locked {
    conn: Option<PoolConnection<MySql>>,
    name: String,
}

impl Locked {
    /// Takes the named lock `name` on `conn`, waiting up to `wait_secs`
    /// (0: not at all); `None` when another session holds it.
    pub async fn acquire(
        mut conn: PoolConnection<MySql>,
        name: String,
        wait_secs: u64,
    ) -> Result<Option<Self>, sqlx::Error> {
        let got: Option<i64> = sqlx::query_scalar("SELECT GET_LOCK(?, ?)")
            .bind(&name)
            .bind(wait_secs)
            .fetch_one(&mut *conn)
            .await?;
        if got == Some(1) {
            Ok(Some(Self {
                conn: Some(conn),
                name,
            }))
        } else {
            Ok(None)
        }
    }

    /// The connection holding the lock.
    pub fn conn(&mut self) -> &mut MySqlConnection {
        self.conn
            .as_deref_mut()
            .unwrap_or_else(|| unreachable!("a lock guard holds its connection until released"))
    }

    /// Frees the lock and returns the connection to the pool.
    pub async fn release(mut self) -> Result<(), sqlx::Error> {
        let mut conn = self
            .conn
            .take()
            .unwrap_or_else(|| unreachable!("a lock guard is released once"));
        match sqlx::query("SELECT RELEASE_LOCK(?)")
            .bind(&self.name)
            .execute(&mut *conn)
            .await
        {
            Ok(_) => Ok(()),
            Err(err) => {
                drop(conn.detach());
                Err(err)
            }
        }
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            drop(conn.detach());
        }
    }
}
