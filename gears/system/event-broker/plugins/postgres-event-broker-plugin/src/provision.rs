//! `provision` (`docs/DESIGN.md` §4.1): a compare-and-set on the topic's log
//! id, and every table the topic needs, in one transaction.

use std::collections::HashMap;
use std::sync::Arc;

use event_broker_sdk::backend::support::topic_uuid;
use event_broker_sdk::backend::{
    BackendError, LogClaim, LogId, Partition, Provisioning, TopicSpec,
};
use sqlx::AssertSqlSafe;
use uuid::Uuid;

use crate::error::{self, Operation};
use crate::maintenance::catalog::Segment;
use crate::maintenance::registry::Layout;
use crate::names::{self, MAX_PARTITIONS};
use crate::segment;
use crate::shared::{KnownTopic, Shared};

const OP: Operation = Operation::Provision;

/// Claims the topic's log for `claim` and creates its tables; `LogConflict`
/// when the stored log id is not one the claim accepts.
pub async fn provision(
    shared: &Shared,
    spec: &TopicSpec,
    claim: &LogClaim,
) -> Result<Provisioning, BackendError> {
    if spec.partitions == 0 || spec.partitions > MAX_PARTITIONS {
        return Err(BackendError::invalid_config(format!(
            "a topic has between 1 and {MAX_PARTITIONS} partitions on the postgres backend"
        )));
    }
    let pool = shared.pool()?;
    let topic = topic_uuid(&spec.topic);
    let max_age_secs = spec.retention.max_age.map(|age| age.as_secs_f64());
    let max_bytes = spec
        .retention
        .max_bytes
        .map(|b| i64::try_from(b).unwrap_or(i64::MAX));
    let map = |e| error::map(OP, &e);

    let mut tx = pool.begin().await.map_err(map)?;
    sqlx::query(
        "INSERT INTO evbk__log (topic_uuid, topic_id, log_id, partitions, max_age, max_bytes, provisioned_at) \
         VALUES ($1, $2, $3, $4, make_interval(secs => $5), $6, now()) ON CONFLICT (topic_uuid) DO NOTHING",
    )
    .bind(topic)
    .bind(spec.topic.as_ref())
    .bind(claim.new.uuid())
    .bind(i32::try_from(spec.partitions).unwrap_or(i32::MAX))
    .bind(max_age_secs)
    .bind(max_bytes)
    .execute(&mut *tx)
    .await
    .map_err(map)?;
    let (stored, partitions): (Uuid, i32) =
        sqlx::query_as("SELECT log_id, partitions FROM evbk__log WHERE topic_uuid = $1 FOR UPDATE")
            .bind(topic)
            .fetch_one(&mut *tx)
            .await
            .map_err(map)?;
    if !claim.accepts(Some(LogId::from_uuid(stored))) {
        tx.rollback().await.map_err(map)?;
        return Err(BackendError::log_conflict(OP.name()));
    }
    // Fresh, resumed or replacing - either way the row takes the claim's id and
    // the spec's current retention.
    sqlx::query(
        "UPDATE evbk__log SET log_id = $2, max_age = make_interval(secs => $3), max_bytes = $4 WHERE topic_uuid = $1",
    )
    .bind(topic)
    .bind(claim.new.uuid())
    .bind(max_age_secs)
    .bind(max_bytes)
    .execute(&mut *tx)
    .await
    .map_err(map)?;

    // A table keeps the name it was created with, whatever the naming rules
    // say now.
    let stored: HashMap<i32, String> =
        sqlx::query_as("SELECT partition, table_name FROM evbk__partition WHERE topic_uuid = $1")
            .bind(topic)
            .fetch_all(&mut *tx)
            .await
            .map_err(map)?
            .into_iter()
            .collect();

    let partitions = u32::try_from(partitions).unwrap_or(0);
    let span = segment::span(shared.settings.segment, None);
    let mut created = Vec::new();
    let mut tables = Vec::new();
    for partition in (0..partitions).map(Partition::new) {
        let parent = stored
            .get(&names::partition_column(partition))
            .cloned()
            .unwrap_or_else(|| names::parent(topic, partition));
        // Segments only with a new table: one that has been written to keeps
        // the segments it has.
        let fresh: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NULL")
            .bind(&parent)
            .fetch_one(&mut *tx)
            .await
            .map_err(map)?;
        let mut statements = format!(
            "CREATE TABLE IF NOT EXISTS {parent} (\
                 sequence BIGINT NOT NULL, \
                 sequence_time TIMESTAMPTZ NOT NULL, \
                 tenant_id UUID NOT NULL, \
                 body BYTEA NOT NULL, \
                 PRIMARY KEY (sequence)\
             ) PARTITION BY RANGE (sequence); \
             INSERT INTO evbk__partition (topic_uuid, partition, table_name) VALUES ('{topic}', {partition}, '{parent}') ON CONFLICT DO NOTHING; \
             INSERT INTO evbk__retention (topic_uuid, partition) VALUES ('{topic}', {partition}) ON CONFLICT DO NOTHING; "
        );
        if fresh {
            // `sequence_time` rises with `sequence` (`append`), the order a
            // block-range index summarizes best: `resolve(At)` skips whole
            // ranges of blocks for a few bytes per range. Only on a new table:
            // `CREATE INDEX` takes a SHARE lock on the table and every segment
            // even when the index exists, and would wait for every open append.
            let time_index = names::time_index(&parent);
            let ranges = segment::ahead(0, &[], span);
            let index = format!("CREATE INDEX {time_index} ON {parent} USING brin (sequence_time)");
            let ddl: Vec<String> = std::iter::once(index)
                .chain(ranges
                .iter()
                .map(|&(first, end)| {
                    let name = names::segment(&parent, first);
                    format!(
                        "CREATE TABLE {name} PARTITION OF {parent} FOR VALUES FROM ({first}) TO ({end}); \
                         INSERT INTO evbk__segment (topic_uuid, partition, first_seq, end_seq, table_name, state, changed_at) \
                         VALUES ('{topic}', {partition}, {first}, {end}, '{name}', 'open', now())"
                    )
                }))
                .collect();
            statements.push_str(&ddl.join("; "));
            created.push((
                (topic, partition),
                ranges
                    .into_iter()
                    .map(|(first, end)| Segment::open(first, end))
                    .collect::<Vec<_>>(),
            ));
        }
        sqlx::raw_sql(AssertSqlSafe(statements))
            .execute(&mut *tx)
            .await
            .map_err(map)?;
        tables.push(parent);
    }
    tx.commit().await.map_err(map)?;

    for (key, segments) in created {
        shared
            .maintenance
            .registry
            .replace(key, Layout::new(&segments));
    }
    shared.remember(KnownTopic {
        id: spec.topic.clone(),
        uuid: topic,
        partitions,
        tables: tables.into_iter().map(Arc::from).collect(),
    });
    Ok(Provisioning::Ready { partitions })
}
