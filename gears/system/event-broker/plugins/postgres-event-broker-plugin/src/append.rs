//! `append` (`docs/DESIGN.md` §4.2): fence first, then a row lock per touched
//! partition in ascending order, the events, and the head notification - one
//! transaction.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use event_broker_sdk::Sequence;
use event_broker_sdk::backend::support::partition_for;
use event_broker_sdk::backend::{
    AppendRequest, Appended, BackendError, Head, KeyedEvent, Partition, TopicPartition,
};
use sqlx::AssertSqlSafe;
use uuid::Uuid;

use crate::settings::SegmentRule;
use crate::shared::Shared;
use crate::{body, error, names};

pub const MAX_EVENTS_PER_APPEND: usize = 1000;

const OP: &str = "append";

pub async fn append(
    shared: &Shared,
    request: &AppendRequest<'_>,
) -> Result<Appended, BackendError> {
    if request.events.len() > MAX_EVENTS_PER_APPEND {
        return Err(BackendError::append_failed(format!(
            "an append carries at most {MAX_EVENTS_PER_APPEND} events"
        )));
    }
    let pool = shared.pool()?;
    let map = |e| error::map(OP, &e);
    let mut tx = pool.begin().await.map_err(map)?;

    // (1) The fence, first: it takes the fence key's row lock, so a retry of an
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

    // (2) Provisioned?
    let (topic, known) = shared.topic(request.topic, OP).await?;

    let mut by_partition: BTreeMap<Partition, Vec<&KeyedEvent>> = BTreeMap::new();
    for keyed in request.events {
        by_partition
            .entry(partition_for(&keyed.key, known.partitions))
            .or_default()
            .push(keyed);
    }

    let mut heads = Vec::with_capacity(by_partition.len());
    let mut crossed = false;
    for (partition, events) in by_partition {
        let bodies = events
            .iter()
            .map(|keyed| body::encode(&keyed.event))
            .collect::<Result<Vec<_>, _>>()?;
        let count = i64::try_from(events.len()).unwrap_or(i64::MAX);
        let bytes: i64 = bodies
            .iter()
            .map(|b| i64::try_from(b.len()).unwrap_or(i64::MAX))
            .sum();

        // (3) Sequences, under the partition's row lock until commit; the
        // storage time is read after the lock, so it rises with the sequence.
        let (last, stored_at): (i64, DateTime<Utc>) = sqlx::query_as(
            "UPDATE evbk__partition SET last_sequence = last_sequence + $3, \
             appended_bytes = appended_bytes + $4 \
             WHERE topic_uuid = $1 AND partition = $2 RETURNING last_sequence, clock_timestamp()",
        )
        .bind(topic)
        .bind(names::partition_column(partition))
        .bind(count)
        .bind(bytes)
        .fetch_one(&mut *tx)
        .await
        .map_err(map)?;
        let first = last - count + 1;
        crossed |= crosses_half(shared.settings.segment, first - 1, last);

        // (4) The events; PostgreSQL routes each row to its segment.
        insert(&mut tx, topic, partition, first, stored_at, &events, bodies).await?;

        // (5) Wake watchers on commit, in commit order.
        sqlx::query("SELECT pg_notify($1, $2)")
            .bind(names::HEAD_CHANNEL)
            .bind(names::head_payload(topic, partition, last))
            .execute(&mut *tx)
            .await
            .map_err(map)?;
        heads.push(Head {
            at: TopicPartition::new(request.topic.clone(), partition),
            sequence: Sequence::assigned(last),
        });
    }
    tx.commit().await.map_err(map)?;

    for head in &heads {
        shared.heads.publish(head.clone());
    }
    if crossed {
        shared.maintenance.notify_one();
    }
    Ok(Appended::Heads(heads))
}

async fn insert(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    topic: Uuid,
    partition: Partition,
    first: i64,
    stored_at: DateTime<Utc>,
    events: &[&KeyedEvent],
    bodies: Vec<Vec<u8>>,
) -> Result<(), BackendError> {
    let sequences: Vec<i64> = (first..).take(events.len()).collect();
    let occurred: Vec<DateTime<Utc>> = events.iter().map(|k| k.event.occurred_at).collect();
    let types: Vec<String> = events
        .iter()
        .map(|k| k.event.type_id.as_ref().to_owned())
        .collect();
    let subject_types: Vec<String> = events
        .iter()
        .map(|k| k.event.subject_type.as_ref().to_owned())
        .collect();
    let tenants: Vec<Uuid> = events.iter().map(|k| k.event.tenant_id).collect();
    let sizes: Vec<i32> = bodies
        .iter()
        .map(|b| i32::try_from(b.len()).unwrap_or(i32::MAX))
        .collect();
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {} (sequence, sequence_time, occurred_at, type_id, subject_type, tenant_id, stored_bytes, body) \
         SELECT s, $2, o, t, st, tn, sz, b FROM UNNEST($1::bigint[], $3::timestamptz[], $4::varchar[], \
         $5::varchar[], $6::uuid[], $7::int[], $8::bytea[]) AS x(s, o, t, st, tn, sz, b)",
        names::parent(topic, partition)
    )))
    .bind(sequences)
    .bind(stored_at)
    .bind(occurred)
    .bind(types)
    .bind(subject_types)
    .bind(tenants)
    .bind(sizes)
    .bind(bodies)
    .execute(&mut **tx)
    .await
    .map_err(|e| error::map(OP, &e))?;
    Ok(())
}

/// Whether going from `before` to `last` crossed a half-segment boundary, so
/// the maintenance worker should create the next segment now. Known only under
/// an `events` rule; a `period` rule leaves it to the worker's cadence.
fn crosses_half(rule: SegmentRule, before: i64, last: i64) -> bool {
    match rule {
        SegmentRule::Events(span) => {
            let half = i64::try_from(span >> 1).unwrap_or(i64::MAX).max(1);
            before.div_euclid(half) != last.div_euclid(half)
        }
        SegmentRule::Period(_) => false,
    }
}
