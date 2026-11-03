//! `provision` (`docs/DESIGN.md` §4.1): a compare-and-set on the topic's log
//! id, the retention bounds and a row per partition, in one write
//! transaction.

use chrono::Utc;
use event_broker_sdk::backend::support::topic_uuid;
use event_broker_sdk::backend::{BackendError, LogClaim, LogId, Provisioning, TopicSpec};
use uuid::Uuid;

use crate::clock;
use crate::error::{self, Operation};
use crate::shared::{KnownTopic, Shared};

/// The most partitions a topic has: the `PostgreSQL` plugin's limit, so a
/// topic fits on either.
pub const MAX_PARTITIONS: u32 = 9999;

const OP: Operation = Operation::Provision;

/// Records the topic under `claim`'s log id and the spec's retention;
/// `LogConflict` when the stored log is one `claim` does not accept.
pub async fn provision(
    shared: &Shared,
    spec: &TopicSpec,
    claim: &LogClaim,
) -> Result<Provisioning, BackendError> {
    if spec.partitions == 0 || spec.partitions > MAX_PARTITIONS {
        return Err(BackendError::invalid_config(format!(
            "a topic has between 1 and {MAX_PARTITIONS} partitions on the sqlite backend"
        )));
    }
    let opened = shared.opened()?;
    let topic = topic_uuid(&spec.topic);
    let max_age_ms = spec
        .retention
        .max_age
        .map(|age| i64::try_from(age.as_millis()).unwrap_or(i64::MAX));
    let max_bytes = spec
        .retention
        .max_bytes
        .map(|b| i64::try_from(b).unwrap_or(i64::MAX));
    let map = |e| error::map(OP, &e);

    let _writer = opened.hub.writer.lock().await;
    let mut tx = opened
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(map)?;
    sqlx::query(
        "INSERT INTO evbk__log (topic_uuid, topic_id, log_id, partitions, max_age_ms, max_bytes, provisioned_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) ON CONFLICT (topic_uuid) DO NOTHING",
    )
    .bind(topic)
    .bind(spec.topic.as_ref())
    .bind(claim.new.uuid())
    .bind(i64::from(spec.partitions))
    .bind(max_age_ms)
    .bind(max_bytes)
    .bind(clock::micros(Utc::now()))
    .execute(&mut *tx)
    .await
    .map_err(map)?;
    let (stored, partitions): (Uuid, i64) =
        sqlx::query_as("SELECT log_id, partitions FROM evbk__log WHERE topic_uuid = ?1")
            .bind(topic)
            .fetch_one(&mut *tx)
            .await
            .map_err(map)?;
    if !claim.accepts(Some(LogId::from_uuid(stored))) {
        tx.rollback().await.map_err(map)?;
        return Err(BackendError::log_conflict(OP.name()));
    }
    // Fresh, resumed or replacing - either way the row takes the claim's id
    // and the spec's current retention, which the sweep reads on its next pass.
    sqlx::query(
        "UPDATE evbk__log SET log_id = ?2, max_age_ms = ?3, max_bytes = ?4 WHERE topic_uuid = ?1",
    )
    .bind(topic)
    .bind(claim.new.uuid())
    .bind(max_age_ms)
    .bind(max_bytes)
    .execute(&mut *tx)
    .await
    .map_err(map)?;
    // The count the log holds, not the spec's: a pre-existing log keeps its own.
    let partitions = u32::try_from(partitions).map_err(|_| {
        BackendError::Internal(format!(
            "{OP}: evbk__log holds a partition count out of range"
        ))
    })?;
    sqlx::query(
        "WITH RECURSIVE p(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM p WHERE n + 1 < ?2) \
         INSERT INTO evbk__partition (topic_uuid, partition) SELECT ?1, n FROM p WHERE true \
         ON CONFLICT (topic_uuid, partition) DO NOTHING",
    )
    .bind(topic)
    .bind(i64::from(partitions))
    .execute(&mut *tx)
    .await
    .map_err(map)?;
    tx.commit().await.map_err(map)?;

    shared.remember(
        &spec.topic,
        KnownTopic {
            uuid: topic,
            partitions,
        },
    );
    Ok(Provisioning::Ready { partitions })
}
