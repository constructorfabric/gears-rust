//! `append` (`docs/DESIGN.md` §4.2): the topic checked, then the fence, a row
//! lock per touched partition in ascending order, and the events - one
//! transaction - and one notification of every new head, in it or after it
//! (`head_notify`).

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use event_broker_sdk::Sequence;
use event_broker_sdk::backend::support::partition_for;
use event_broker_sdk::backend::{
    AppendRequest, Appended, BackendError, Head, KeyedEvent, Partition, TopicPartition,
};
use sqlx::AssertSqlSafe;
use tokio::task::JoinHandle;
use uuid::Uuid;

use crate::error::Operation;
use crate::settings::HeadNotify;
use crate::shared::Shared;
use crate::{body, error, names, notify};

/// The most events one `append` carries.
pub const MAX_EVENTS_PER_APPEND: usize = 1000;

const OP: Operation = Operation::Append;

/// Appends a batch in one transaction, or reports it a duplicate when the fence
/// has already passed its sequence.
pub async fn append(
    shared: &Arc<Shared>,
    request: &AppendRequest<'_>,
) -> Result<Appended, BackendError> {
    if request.events.len() > MAX_EVENTS_PER_APPEND {
        return Err(BackendError::append_failed(format!(
            "an append carries at most {MAX_EVENTS_PER_APPEND} events"
        )));
    }
    let pool = shared.pool()?;
    let map = |e| error::map(OP, &e);

    // (1) Provisioned? Before the fence: a fence that has passed this sequence
    // is no answer for a topic the backend does not hold. A topic is never
    // removed, so the answer holds for the transaction.
    let known = shared.topic(request.topic, OP).await?;
    let topic = known.uuid;

    let mut tx = pool.begin().await.map_err(map)?;

    // (2) The fence, first in the transaction: it takes the fence key's row lock, so a retry of an
    // open call waits for it and then sees what it committed.
    let fenced = sqlx::query(
        "INSERT INTO evbk__fence (fence_key, last_seq) VALUES ($1, $2) \
         ON CONFLICT (fence_key) DO UPDATE SET last_seq = EXCLUDED.last_seq \
         WHERE evbk__fence.last_seq < EXCLUDED.last_seq",
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

    let mut by_partition: BTreeMap<Partition, Vec<&KeyedEvent>> = BTreeMap::new();
    for keyed in request.events {
        by_partition
            .entry(partition_for(&keyed.key, known.partitions))
            .or_default()
            .push(keyed);
    }

    let mut heads = Vec::with_capacity(by_partition.len());
    for (partition, events) in by_partition {
        let bodies = events
            .iter()
            .map(|keyed| body::encode(&keyed.event))
            .collect::<Result<Vec<_>, _>>()?;
        let count = i64::try_from(events.len()).unwrap_or(i64::MAX);

        // (3) Sequences, under the partition's row lock until commit; the
        // storage time is read after the lock, so it rises with the sequence.
        let (last, table, stored_at): (i64, String, DateTime<Utc>) = sqlx::query_as(
            "UPDATE evbk__partition SET hwm = hwm + $3 WHERE topic_uuid = $1 AND partition = $2 \
             RETURNING hwm, table_name, clock_timestamp()",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .bind(count)
        .fetch_one(&mut *tx)
        .await
        .map_err(map)?;
        let first = last - count + 1;

        // (4) The events; PostgreSQL routes each row to its segment. No
        // segment for a sequence means the extender fell behind: it is
        // called at once, and the caller retries.
        insert(&mut tx, &table, first, stored_at, &events, bodies)
            .await
            .map_err(|e| {
                if is_no_segment(&e) {
                    shared.maintenance.no_room((topic, partition));
                }
                map(e)
            })?;
        heads.push(Head {
            at: TopicPartition::new(request.topic.clone(), partition),
            sequence: Sequence::assigned(last),
        });
    }

    // (5) Wake the other instances' watchers: every head in one statement,
    // in the transaction or after its commit.
    let notified: Vec<(Uuid, Partition, i64)> = heads
        .iter()
        .map(|head| (topic, head.at.partition, head.sequence.as_i64()))
        .collect();
    let head_notify = shared.settings.head_notify;
    let commit = {
        let notified = notified.clone();
        async move {
            if head_notify == HeadNotify::InTransaction {
                // Delivered on commit, in commit order.
                notify::send(&mut *tx, &notified).await?;
            }
            tx.commit().await
        }
    };
    let publish = {
        let shared = Arc::clone(shared);
        let heads = heads.clone();
        move || {
            for &(topic, partition, sequence) in &notified {
                if head_notify == HeadNotify::AfterCommit {
                    shared.heads_to_notify.offer(topic, partition, sequence);
                }
                shared.maintenance.appended((topic, partition), sequence);
            }
            for head in heads {
                shared.heads.publish(head);
            }
        }
    };
    finish(commit, publish)
        .await
        .map_err(|_| BackendError::Internal(format!("{OP}: the commit task ended abnormally")))?
        .map_err(map)?;
    Ok(Appended::Heads(heads))
}

/// Runs `commit` and then, once it has landed, `publish`, in a task of its
/// own started at once: a caller that goes away once the commit is issued
/// leaves it running, so a commit that lands always has its heads published
/// to the local hub and, under `after_commit`, offered to the notify worker -
/// else a quiet partition would wait for its next append.
pub fn finish<C, P>(commit: C, publish: P) -> JoinHandle<Result<(), sqlx::Error>>
where
    C: Future<Output = Result<(), sqlx::Error>> + Send + 'static,
    P: FnOnce() + Send + 'static,
{
    tokio::spawn(async move {
        commit.await?;
        publish();
        Ok(())
    })
}

async fn insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table: &str,
    first: i64,
    stored_at: DateTime<Utc>,
    events: &[&KeyedEvent],
    bodies: Vec<Vec<u8>>,
) -> Result<(), sqlx::Error> {
    let sequences: Vec<i64> = (first..).take(events.len()).collect();
    let tenants: Vec<Uuid> = events.iter().map(|k| k.event.tenant_id).collect();
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {table} (sequence, sequence_time, tenant_id, body) \
         SELECT s, $2, tn, b FROM UNNEST($1::bigint[], $3::uuid[], $4::bytea[]) AS x(s, tn, b)"
    )))
    .bind(sequences)
    .bind(stored_at)
    .bind(tenants)
    .bind(bodies)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// `check_violation` from the parent table: no segment covers a sequence.
fn is_no_segment(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("23514"))
}
