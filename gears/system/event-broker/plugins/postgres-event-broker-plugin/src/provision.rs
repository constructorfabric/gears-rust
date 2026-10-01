//! `provision` (`docs/DESIGN.md` §4.1): a compare-and-set on the topic's log
//! id, and every table the topic needs, in one transaction.

use event_broker_sdk::backend::support::topic_uuid;
use event_broker_sdk::backend::{
    BackendError, LogClaim, LogId, Partition, Provisioning, TopicSpec,
};
use sqlx::AssertSqlSafe;
use uuid::Uuid;

use crate::error;
use crate::names::{self, MAX_PARTITIONS};
use crate::segment;
use crate::shared::{KnownTopic, Shared};

const OP: &str = "provision";

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
        return Err(BackendError::log_conflict(OP));
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

    let partitions = u32::try_from(partitions).unwrap_or(0);
    let span = segment::span(shared.settings.segment, None);
    for partition in (0..partitions).map(Partition::new) {
        let parent = names::parent(topic, partition);
        let first = names::segment(topic, partition, 1);
        let index = names::occurred_index(topic, partition);
        sqlx::raw_sql(AssertSqlSafe(format!(
            "CREATE TABLE IF NOT EXISTS {parent} (\
                 sequence BIGINT NOT NULL, \
                 sequence_time TIMESTAMPTZ NOT NULL, \
                 occurred_at TIMESTAMPTZ NOT NULL, \
                 type_id VARCHAR(1024) NOT NULL, \
                 subject_type VARCHAR(1024) NOT NULL, \
                 tenant_id UUID NOT NULL, \
                 stored_bytes INTEGER NOT NULL, \
                 body BYTEA NOT NULL, \
                 PRIMARY KEY (sequence)\
             ) PARTITION BY RANGE (sequence); \
             CREATE INDEX IF NOT EXISTS {index} ON {parent} (occurred_at); \
             CREATE TABLE IF NOT EXISTS {first} PARTITION OF {parent} FOR VALUES FROM (1) TO ({end}); \
             INSERT INTO evbk__partition (topic_uuid, partition) VALUES ('{topic}', {partition}) ON CONFLICT DO NOTHING; \
             INSERT INTO evbk__retention (topic_uuid, partition) VALUES ('{topic}', {partition}) ON CONFLICT DO NOTHING",
            end = 1 + span,
        )))
        .execute(&mut *tx)
        .await
        .map_err(map)?;
    }
    tx.commit().await.map_err(map)?;

    shared.remember(
        topic,
        KnownTopic {
            id: spec.topic.clone(),
            partitions,
        },
    );
    shared.maintenance.notify_one();
    Ok(Provisioning::Ready { partitions })
}
