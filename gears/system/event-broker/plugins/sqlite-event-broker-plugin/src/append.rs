//! `append` (`docs/DESIGN.md` §4.2): the topic checked, then under the file's
//! write lock the fence, each touched partition's head raised and its events
//! inserted - one transaction - and the new heads published once it commits.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use event_broker_sdk::Sequence;
use event_broker_sdk::backend::support::partition_for;
use event_broker_sdk::backend::{
    AppendRequest, Appended, BackendError, Head, Partition, TopicPartition,
};
use sqlx::{QueryBuilder, Sqlite, Transaction};
use tokio::sync::OwnedMutexGuard;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::{self, Operation};
use crate::hub::Hub;
use crate::shared::{Shared, column};
use crate::{body, clock};

/// The most events one `append` carries: seven bound values each stay well
/// under `SQLite`'s 32766 per statement.
pub const MAX_EVENTS_PER_APPEND: usize = 1000;

const OP: Operation = Operation::Append;

/// One event as it is inserted: its tenant and its encoded body.
struct Row {
    tenant_id: Uuid,
    body: Vec<u8>,
}

/// One partition's share of an append, encoded before the write lock is taken.
#[derive(Default)]
struct Batch {
    rows: Vec<Row>,
    bytes: i64,
}

/// Up to `COMMIT` the caller's future owns the transaction: dropped before
/// then, it rolls back and nothing is stored (feature 0007 §2.2). From
/// `COMMIT` on the write runs detached ([`finish`]).
pub async fn append(
    shared: &Shared,
    request: &AppendRequest<'_>,
) -> Result<Appended, BackendError> {
    if request.events.len() > MAX_EVENTS_PER_APPEND {
        return Err(BackendError::append_failed(format!(
            "an append carries at most {MAX_EVENTS_PER_APPEND} events"
        )));
    }
    let opened = shared.opened()?;
    let map = |e| error::map(OP, &e);

    // (1) Provisioned? Before the fence: a fence that has passed this sequence
    // is no answer for a topic the backend does not hold.
    let known = shared.topic(request.topic, OP).await?;

    let mut batches: BTreeMap<Partition, Batch> = BTreeMap::new();
    for keyed in request.events {
        let encoded = body::encode(&keyed.event)?;
        let batch = batches
            .entry(partition_for(&keyed.key, known.partitions))
            .or_default();
        batch.bytes += i64::try_from(encoded.len()).unwrap_or(i64::MAX);
        batch.rows.push(Row {
            tenant_id: keyed.event.tenant_id,
            body: encoded,
        });
    }

    // (2) One writer at a time: `BEGIN IMMEDIATE` takes the file's write lock
    // up front, so no read-to-write upgrade can fail midway.
    let writer = Arc::clone(&opened.hub.writer).lock_owned().await;
    let mut tx = opened
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(map)?;

    // (3) The fence: applied at most once, in the transaction that stores the
    // events.
    let fenced = sqlx::query(
        "INSERT INTO evbk__fence (fence_key, last_seq) VALUES (?1, ?2) \
         ON CONFLICT (fence_key) DO UPDATE SET last_seq = excluded.last_seq \
         WHERE evbk__fence.last_seq < excluded.last_seq",
    )
    .bind(request.fence.key.uuid())
    .bind(request.fence.seq.value())
    .execute(&mut *tx)
    .await
    .map_err(map)?;
    if fenced.rows_affected() == 0 {
        tx.rollback().await.map_err(map)?;
        return Ok(Appended::Duplicate);
    }

    // (4) Sequences and the storage time per partition: the head rises by the
    // batch, and the time never falls below the head's, so `sequence_time`
    // rises with `sequence` even when the clock steps back.
    let now = clock::micros(Utc::now());
    let mut heads = Vec::with_capacity(batches.len());
    for (partition, batch) in batches {
        let count = i64::try_from(batch.rows.len()).unwrap_or(i64::MAX);
        let (last, stored_at): (i64, i64) = sqlx::query_as(
            "UPDATE evbk__partition SET hwm = hwm + ?3, bytes = bytes + ?4, head_time = MAX(head_time, ?5) \
             WHERE topic_uuid = ?1 AND partition = ?2 RETURNING hwm, head_time",
        )
        .bind(known.uuid)
        .bind(column(partition))
        .bind(count)
        .bind(batch.bytes)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(map)?;
        insert(
            &mut tx,
            known.uuid,
            partition,
            last - count + 1,
            stored_at,
            batch.rows,
        )
        .await
        .map_err(map)?;
        heads.push(Head {
            at: TopicPartition::new(request.topic.clone(), partition),
            sequence: Sequence::assigned(last),
        });
    }

    // (5) The commit and the publish.
    finish(tx, writer, Arc::clone(&opened.hub), heads)
        .await
        .map_err(|_| BackendError::Internal(format!("{OP}: the commit task ended abnormally")))?
}

/// Commits `tx`, releases the write turn and publishes `heads` to the hub, in
/// a task of its own started at once: a caller that goes away once the commit
/// is issued leaves it running, so a commit that lands always has its heads
/// published - else native watches would miss them until the partition's next
/// append.
pub fn finish(
    tx: Transaction<'static, Sqlite>,
    writer: OwnedMutexGuard<()>,
    hub: Arc<Hub>,
    heads: Vec<Head>,
) -> JoinHandle<Result<Appended, BackendError>> {
    tokio::spawn(async move {
        tx.commit().await.map_err(|e| error::map(OP, &e))?;
        drop(writer);
        for head in &heads {
            hub.heads.publish(head.clone());
        }
        Ok(Appended::Heads(heads))
    })
}

async fn insert(
    tx: &mut Transaction<'static, Sqlite>,
    topic: Uuid,
    partition: Partition,
    first: i64,
    stored_at: i64,
    rows: Vec<Row>,
) -> Result<(), sqlx::Error> {
    let mut insert = QueryBuilder::<Sqlite>::new(
        "INSERT INTO evbk__event (topic_uuid, partition, sequence, sequence_time, tenant_id, size, body) ",
    );
    insert.push_values((first..).zip(rows), |mut values, (sequence, row)| {
        values
            .push_bind(topic)
            .push_bind(column(partition))
            .push_bind(sequence)
            .push_bind(stored_at)
            .push_bind(row.tenant_id)
            .push_bind(i64::try_from(row.body.len()).unwrap_or(i64::MAX))
            .push_bind(row.body);
    });
    insert.build().execute(&mut **tx).await?;
    Ok(())
}
