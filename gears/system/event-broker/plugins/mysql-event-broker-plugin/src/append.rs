//! `append` (`docs/DESIGN.md` §4.2): the topic checked, then the fence, a row
//! lock per touched partition in ascending order, and the events - one
//! transaction.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use event_broker_sdk::Sequence;
use event_broker_sdk::backend::support::partition_for;
use event_broker_sdk::backend::{
    AppendRequest, Appended, BackendError, Fence, Head, KeyedEvent, Partition, TopicPartition,
};
use sqlx::{AssertSqlSafe, MySql, MySqlConnection, Transaction};
use uuid::Uuid;

use crate::error::{self, Operation};
use crate::shared::Shared;
use crate::{body, names, segment};

/// The most events one `append` carries.
pub const MAX_EVENTS_PER_APPEND: usize = 1000;

/// The most body bytes one `INSERT` carries, well under the smallest default
/// `max_allowed_packet` of the supported servers (16 MiB on `MariaDB`).
const MAX_INSERT_BYTES: usize = 4 << 20;

/// `ER_DUP_ENTRY`.
const DUPLICATE_KEY: u16 = 1062;

const OP: Operation = Operation::Append;

/// Appends a batch in one transaction, or reports it a duplicate when the fence
/// has already passed its sequence.
pub async fn append(
    shared: &Shared,
    request: &AppendRequest<'_>,
) -> Result<Appended, BackendError> {
    if request.events.len() > MAX_EVENTS_PER_APPEND {
        return Err(BackendError::append_failed(format!(
            "an append carries at most {MAX_EVENTS_PER_APPEND} events"
        )));
    }
    shared.pool()?;
    let map = |e: sqlx::Error| error::map(OP, &e);

    // (1) Provisioned? Before the fence: a fence that has passed this sequence
    // is no answer for a topic the backend does not hold. A topic is never
    // removed, so the answer holds for the transaction.
    let known = shared.topic(request.topic, OP).await?;
    let topic = known.uuid;

    let mut by_partition: BTreeMap<Partition, Vec<&KeyedEvent>> = BTreeMap::new();
    for keyed in request.events {
        by_partition
            .entry(partition_for(&keyed.key, known.partitions))
            .or_default()
            .push(keyed);
    }
    // Encoded before the transaction opens: no lock is held while it runs.
    let mut batches = Vec::with_capacity(by_partition.len());
    for (partition, events) in by_partition {
        let bodies = events
            .iter()
            .map(|keyed| body::encode(&keyed.event))
            .collect::<Result<Vec<_>, _>>()?;
        let tenants: Vec<Uuid> = events.iter().map(|k| k.event.tenant_id).collect();
        let table = usize::try_from(partition.value())
            .ok()
            .and_then(|i| known.tables.get(i))
            .ok_or_else(|| BackendError::Internal(format!("{OP}: no table for a partition")))?;
        batches.push(Batch {
            partition,
            table: Arc::clone(table),
            tenants,
            bodies,
        });
    }

    let stored = error::retried(|| store(shared, request, topic, &batches))
        .await
        .map_err(map)?;
    let Some(heads) = stored else {
        return Ok(Appended::Duplicate);
    };
    for head in &heads {
        shared
            .maintenance
            .appended((topic, head.at.partition), head.sequence.as_i64());
        shared.heads.publish(head.clone());
    }
    Ok(Appended::Heads(heads))
}

/// One partition's share of an append, encoded.
struct Batch {
    partition: Partition,
    table: Arc<str>,
    tenants: Vec<Uuid>,
    bodies: Vec<Vec<u8>>,
}

/// The fence and every batch in one transaction: the heads it assigned, or
/// `None` when the fence was applied already. Run again whole when chosen as
/// a deadlock victim.
async fn store(
    shared: &Shared,
    request: &AppendRequest<'_>,
    topic: Uuid,
    batches: &[Batch],
) -> Result<Option<Vec<Head>>, sqlx::Error> {
    let mut tx = shared
        .pool()
        .map_err(|_| sqlx::Error::PoolClosed)?
        .begin()
        .await?;

    // (2) The fence, first in the transaction: it takes the fence key's row
    // lock, so a retry of an open call waits for it and then sees what it
    // committed.
    if !apply_fence(&mut tx, request.fence).await? {
        tx.rollback().await?;
        return Ok(None);
    }

    let mut heads = Vec::with_capacity(batches.len());
    for batch in batches {
        let count = i64::try_from(batch.bodies.len()).unwrap_or(i64::MAX);
        let weights: Vec<i64> = batch
            .bodies
            .iter()
            .map(|b| segment::weight(b.len()))
            .collect();
        let total: i64 = weights.iter().sum();

        // (3) Sequences and the running weight, under the partition's row
        // lock until commit; the storage time is read after the lock, so it
        // rises with the sequence.
        let raised = sqlx::query(
            "UPDATE evbk__partition SET hwm = hwm + ?, bytes = bytes + ? \
             WHERE topic_uuid = ? AND partition_no = ?",
        )
        .bind(count)
        .bind(total)
        .bind(topic)
        .bind(names::partition_column(batch.partition))
        .execute(&mut *tx)
        .await?;
        if raised.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound);
        }
        // The storage time never goes back within a partition: a clock
        // stepped back keeps the last event's time until it passes it, so
        // `resolve(At)` may go on skipping sealed segments by their newest
        // time. The last event is read by its key; it is gone only when
        // retention removed everything, and nothing is left to order against.
        let (last, cumulative, now, previous): (i64, i64, DateTime<Utc>, Option<DateTime<Utc>>) =
            sqlx::query_as(AssertSqlSafe(format!(
                "SELECT p.hwm, p.bytes, UTC_TIMESTAMP(6), \
                 (SELECT e.seq_time FROM {table} e WHERE e.seq = p.hwm - ?) \
                 FROM evbk__partition p WHERE p.topic_uuid = ? AND p.partition_no = ?",
                table = batch.table
            )))
            .bind(count)
            .bind(topic)
            .bind(names::partition_column(batch.partition))
            .fetch_one(&mut *tx)
            .await?;
        let stored_at = previous.map_or(now, |previous| previous.max(now));

        // (4) The events; the server routes each row to its range partition.
        // No range partition for a sequence means the maintenance fell
        // behind: it is offered the partition, and the caller retries.
        let rows = Rows {
            first: last - count + 1,
            cumulative_before: cumulative - total,
            stored_at,
            tenants: &batch.tenants,
            weights: &weights,
            bodies: &batch.bodies,
        };
        insert(&mut tx, &batch.table, &rows)
            .await
            .inspect_err(|e| {
                if error::number(e) == Some(error::NO_PARTITION) {
                    shared.maintenance.no_room((topic, batch.partition));
                }
            })?;
        heads.push(Head {
            at: TopicPartition::new(request.topic.clone(), batch.partition),
            sequence: Sequence::assigned(last),
        });
    }
    tx.commit().await?;
    Ok(Some(heads))
}

/// Applies `fence` under its row lock; `false` when a fence at or past its
/// seq was applied already.
async fn apply_fence(tx: &mut Transaction<'_, MySql>, fence: Fence) -> Result<bool, sqlx::Error> {
    let key = fence.key.uuid();
    let seq = fence.seq.value();
    // Twice at most: a first applier of the same key inserting the row
    // between the read and the insert makes the insert a duplicate, and the
    // second read sees its row.
    for _ in 0..2 {
        let last: Option<i64> =
            sqlx::query_scalar("SELECT last_seq FROM evbk__fence WHERE fence_key = ? FOR UPDATE")
                .bind(key)
                .fetch_optional(&mut **tx)
                .await?;
        match last {
            Some(last) if last >= seq => return Ok(false),
            Some(_) => {
                sqlx::query("UPDATE evbk__fence SET last_seq = ? WHERE fence_key = ?")
                    .bind(seq)
                    .bind(key)
                    .execute(&mut **tx)
                    .await?;
                return Ok(true);
            }
            None => {
                match sqlx::query("INSERT INTO evbk__fence (fence_key, last_seq) VALUES (?, ?)")
                    .bind(key)
                    .bind(seq)
                    .execute(&mut **tx)
                    .await
                {
                    Ok(_) => return Ok(true),
                    Err(err) if error::number(&err) == Some(DUPLICATE_KEY) => {}
                    Err(err) => return Err(err),
                }
            }
        }
    }
    Err(sqlx::Error::Protocol(
        "the fence row neither reads nor inserts".to_owned(),
    ))
}

/// One partition's share of an append, with its positions.
struct Rows<'a> {
    first: i64,
    /// The partition's running weight before the first of these events.
    cumulative_before: i64,
    stored_at: DateTime<Utc>,
    tenants: &'a [Uuid],
    weights: &'a [i64],
    bodies: &'a [Vec<u8>],
}

/// Multi-row `INSERT`s of at most [`MAX_INSERT_BYTES`] of bodies each.
async fn insert(
    conn: &mut MySqlConnection,
    table: &str,
    rows: &Rows<'_>,
) -> Result<(), sqlx::Error> {
    let mut start = 0;
    let mut cumulative = rows.cumulative_before;
    while start < rows.bodies.len() {
        let mut end = start;
        let mut size = 0;
        while end < rows.bodies.len()
            && (end == start || size + rows.bodies[end].len() <= MAX_INSERT_BYTES)
        {
            size += rows.bodies[end].len();
            end += 1;
        }
        let values = vec!["(?, ?, ?, ?, ?)"; end - start].join(", ");
        let mut query = sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO {table} (seq, seq_time, tenant_id, cum_bytes, body) VALUES {values}"
        )));
        for i in start..end {
            cumulative += rows.weights[i];
            query = query
                .bind(rows.first + i64::try_from(i).unwrap_or(i64::MAX))
                .bind(rows.stored_at)
                .bind(rows.tenants[i])
                .bind(cumulative)
                .bind(rows.bodies[i].as_slice());
        }
        query.execute(&mut *conn).await?;
        start = end;
    }
    Ok(())
}
