//! `provision` (`docs/DESIGN.md` §4.1): a compare-and-set on the topic's log
//! id and the topic's catalog rows in one transaction, then each partition's
//! event table, made by a maintenance run under the partition's named lock.
//! The tables come after the commit because DDL commits on its own.

use std::sync::Arc;
use std::time::Duration;

use event_broker_sdk::backend::support::topic_uuid;
use event_broker_sdk::backend::{
    BackendError, LogClaim, LogId, Partition, Provisioning, TopicSpec,
};
use sqlx::MySqlPool;
use uuid::Uuid;

use crate::error::{self, Operation};
use crate::maintenance::steps::{self, Outcome};
use crate::names::{self, MAX_PARTITIONS};
use crate::shared::{KnownTopic, Shared};

const OP: Operation = Operation::Provision;

/// How long `provision` waits for a partition's named lock before it answers
/// `Pending`, in seconds.
const LOCK_WAIT: u64 = 5;

/// The longest age bound stored: longer is no bound in practice, and keeps
/// `now - max_age` inside `DATETIME`'s range.
const MAX_AGE_CAP: Duration = Duration::from_hours(100 * 365 * 24);

/// Claims the topic's log for `claim` and creates its tables; `LogConflict`
/// when the stored log id is not one the claim accepts.
pub async fn provision(
    shared: &Shared,
    spec: &TopicSpec,
    claim: &LogClaim,
) -> Result<Provisioning, BackendError> {
    if spec.partitions == 0 || spec.partitions > MAX_PARTITIONS {
        return Err(BackendError::invalid_config(format!(
            "a topic has between 1 and {MAX_PARTITIONS} partitions on the mysql backend"
        )));
    }
    let pool = shared.pool()?;
    let topic = topic_uuid(&spec.topic);
    let max_age_ms = spec
        .retention
        .max_age
        .map(|age| i64::try_from(age.min(MAX_AGE_CAP).as_millis()).unwrap_or(i64::MAX));
    let max_bytes = spec
        .retention
        .max_bytes
        .map(|b| i64::try_from(b).unwrap_or(i64::MAX));
    let map = |e| error::map(OP, &e);

    let Some((partitions, tables)) =
        error::retried(|| claim_log(pool, spec, claim, topic, (max_age_ms, max_bytes)))
            .await
            .map_err(map)?
    else {
        return Err(BackendError::log_conflict(OP.name()));
    };

    // The tables. A partition whose table exists is left to the maintenance:
    // a re-provision of a written topic takes no lock and waits for nothing.
    for (partition, table) in (0..partitions).map(Partition::new).zip(&tables) {
        let exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM information_schema.TABLES WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ?",
        )
        .bind(table)
        .fetch_one(pool)
        .await
        .map_err(map)?;
        if exists > 0 {
            continue;
        }
        match steps::run(shared, pool, (topic, partition), 0, LOCK_WAIT)
            .await
            .map_err(map)?
        {
            Outcome::Done(layout) => shared.maintenance.replace((topic, partition), layout),
            Outcome::Busy => return Ok(Provisioning::Pending),
            Outcome::Gone => {
                return Err(BackendError::Internal(format!(
                    "{OP}: a partition row committed by this call is gone"
                )));
            }
        }
    }

    shared.remember(KnownTopic {
        id: spec.topic.clone(),
        uuid: topic,
        partitions,
        tables: tables.into_iter().map(Arc::from).collect(),
    });
    Ok(Provisioning::Ready { partitions })
}

/// The compare-and-set on the log id and the topic's catalog rows, in one
/// transaction: the partition count held and each partition's table, or
/// `None` when the stored log id refuses the claim.
async fn claim_log(
    pool: &MySqlPool,
    spec: &TopicSpec,
    claim: &LogClaim,
    topic: Uuid,
    (max_age_ms, max_bytes): (Option<i64>, Option<i64>),
) -> Result<Option<(u32, Vec<String>)>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT IGNORE INTO evbk__log \
         (topic_uuid, topic_id, log_id, partitions, max_age_ms, max_bytes, provisioned_at) \
         VALUES (?, ?, ?, ?, ?, ?, UTC_TIMESTAMP(6))",
    )
    .bind(topic)
    .bind(spec.topic.as_ref())
    .bind(claim.new.uuid())
    .bind(i32::try_from(spec.partitions).unwrap_or(i32::MAX))
    .bind(max_age_ms)
    .bind(max_bytes)
    .execute(&mut *tx)
    .await?;
    let (stored, partitions): (Uuid, i32) =
        sqlx::query_as("SELECT log_id, partitions FROM evbk__log WHERE topic_uuid = ? FOR UPDATE")
            .bind(topic)
            .fetch_one(&mut *tx)
            .await?;
    if !claim.accepts(Some(LogId::from_uuid(stored))) {
        tx.rollback().await?;
        return Ok(None);
    }
    // Fresh, resumed or replacing - either way the row takes the claim's id and
    // the spec's current retention.
    sqlx::query(
        "UPDATE evbk__log SET log_id = ?, max_age_ms = ?, max_bytes = ? WHERE topic_uuid = ?",
    )
    .bind(claim.new.uuid())
    .bind(max_age_ms)
    .bind(max_bytes)
    .bind(topic)
    .execute(&mut *tx)
    .await?;

    let partitions = u32::try_from(partitions).unwrap_or(0);
    // A table keeps the name it was created with, whatever the naming rules
    // say now.
    let stored: Vec<(i32, String)> = sqlx::query_as(
        "SELECT partition_no, table_name FROM evbk__partition WHERE topic_uuid = ? ORDER BY partition_no",
    )
    .bind(topic)
    .fetch_all(&mut *tx)
    .await
    ?;
    let mut tables = Vec::with_capacity(stored.len());
    for partition in (0..partitions).map(Partition::new) {
        let column = names::partition_column(partition);
        if let Some((_, table)) = stored.iter().find(|(p, _)| *p == column) {
            tables.push(table.clone());
            continue;
        }
        let table = names::table(topic, partition);
        sqlx::query(
            "INSERT INTO evbk__partition (topic_uuid, partition_no, table_name) VALUES (?, ?, ?)",
        )
        .bind(topic)
        .bind(column)
        .bind(&table)
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO evbk__retention (topic_uuid, partition_no) VALUES (?, ?)")
            .bind(topic)
            .bind(column)
            .execute(&mut *tx)
            .await?;
        tables.push(table);
    }
    tx.commit().await?;
    Ok(Some((partitions, tables)))
}
