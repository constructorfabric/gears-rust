//! Outbox store for the dispatcher: claim, ack, nack.
//!
//! `OutboxStore<E>` is the dispatcher-side API. It creates its own
//! connections/transactions via `DBProvider<E>` (independent of the
//! producer's transaction used by `enqueue`).

use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;

use chrono::Utc;
use rand::Rng;
use sea_orm::{DatabaseBackend, Statement, Value};
use uuid::Uuid;

use super::helpers::{backend, exec, query_all, query_one};
use super::types::{ClaimCfg, ClaimedMessage, RetryCfg};
use crate::secure::DBRunner;
use crate::{DBProvider, DbError};

/// `SQLite` timestamp format with millisecond precision.
/// Must match the `strftime` format used in `SQLite` queries (`%Y-%m-%d %H:%M:%f`).
const SQLITE_TS_FMT: &str = "%Y-%m-%d %H:%M:%S%.3f";

/// Dispatcher-side outbox store.
///
/// Constructed from a [`DBProvider`] scoped to a single namespace.
/// The store creates its own connections and transactions for
/// `claim_batch`, `ack`, and `nack` — independent of any producer transaction.
pub struct OutboxStore<E> {
    db: DBProvider<E>,
    worker_id: Uuid,
    namespace: String,
    retry_cfg: RetryCfg,
    _error: PhantomData<fn() -> E>,
}

impl<E> OutboxStore<E>
where
    E: From<DbError> + Send + 'static,
{
    /// Create a new `OutboxStore`.
    #[must_use]
    pub fn new(db: DBProvider<E>, worker_id: Uuid, namespace: String, retry_cfg: RetryCfg) -> Self {
        Self {
            db,
            worker_id,
            namespace,
            retry_cfg,
            _error: PhantomData,
        }
    }

    /// Returns the namespace this store is scoped to.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Claim a batch of pending outbox rows for publishing.
    ///
    /// Rows are locked using `FOR UPDATE SKIP LOCKED` (Postgres) or
    /// serialised writes (`SQLite`). Claimed rows transition to `processing`
    /// with an incremented `attempts` counter and a lease until `locked_until`.
    ///
    /// # Errors
    ///
    /// Returns `E` if the database operation fails.
    pub async fn claim_batch(&self, cfg: ClaimCfg) -> Result<Vec<ClaimedMessage>, E> {
        let now = Utc::now();
        let locked_until = now
            + chrono::Duration::from_std(cfg.lease_duration)
                .map_err(|e| DbError::Other(anyhow::anyhow!("invalid lease duration: {e}")))?;
        let worker_id = self.worker_id;
        let namespace = self.namespace.clone();
        let max_attempts = self.retry_cfg.max_attempts;
        let batch_size = cfg.batch_size;

        self.db
            .transaction(move |tx| {
                Box::pin(async move {
                    let db_backend = backend(tx);
                    let rows = match db_backend {
                        DatabaseBackend::Postgres => claim_batch_pg(
                            tx,
                            &namespace,
                            worker_id,
                            locked_until,
                            max_attempts,
                            batch_size,
                        )
                        .await
                        .map_err(E::from)?,
                        DatabaseBackend::Sqlite => claim_batch_sqlite(
                            tx,
                            &namespace,
                            worker_id,
                            locked_until,
                            max_attempts,
                            batch_size,
                        )
                        .await
                        .map_err(E::from)?,
                        DatabaseBackend::MySql => {
                            return Err(DbError::InvalidConfig(
                                "Outbox claim is not supported for this database backend".into(),
                            )
                            .into());
                        }
                    };
                    Ok(rows)
                })
                    as Pin<Box<dyn Future<Output = Result<Vec<ClaimedMessage>, E>> + Send + '_>>
            })
            .await
    }

    /// Acknowledge successful delivery of an outbox row.
    ///
    /// Transitions the row to `delivered`. The ack is guarded by `locked_by`:
    /// it only succeeds if this worker still holds the lease.
    ///
    /// # Errors
    ///
    /// Returns `E` if the row is not leased by this worker (e.g., lease
    /// expired and reclaimed by another worker) or on database failure.
    pub async fn ack(&self, id: Uuid) -> Result<(), E> {
        let conn = self.db.conn()?;
        let db_backend = backend(&conn);
        let worker_id = self.worker_id;

        let (sql, values) = match db_backend {
            DatabaseBackend::Postgres => (
                r"UPDATE modkit_outbox_events
                   SET status = 'delivered', updated_at = NOW()
                   WHERE id = $1 AND locked_by = $2 AND status = 'processing'",
                vec![
                    Value::Uuid(Some(Box::new(id))),
                    Value::Uuid(Some(Box::new(worker_id))),
                ],
            ),
            DatabaseBackend::Sqlite => (
                r"UPDATE modkit_outbox_events
                   SET status = 'delivered', updated_at = strftime('%Y-%m-%d %H:%M:%f','now')
                   WHERE id = $1 AND locked_by = $2 AND status = 'processing'",
                vec![
                    Value::from(id.to_string()),
                    Value::from(worker_id.to_string()),
                ],
            ),
            DatabaseBackend::MySql => {
                return Err(
                    DbError::InvalidConfig("Unsupported backend for outbox ack".into()).into(),
                );
            }
        };

        let stmt = Statement::from_sql_and_values(db_backend, sql, values);
        let result = exec(&conn, stmt).await.map_err(DbError::from)?;

        if result.rows_affected() == 0 {
            return Err(DbError::Other(anyhow::anyhow!(
                "outbox ack failed: row {id} is not leased by worker {worker_id} or not in processing state"
            ))
            .into());
        }

        Ok(())
    }

    /// Record a publish failure and schedule retry (or dead-letter).
    ///
    /// If `attempts >= max_attempts`, transitions to `dead`.
    /// Otherwise, returns to `pending` with `next_attempt_at` computed using
    /// exponential backoff with jitter.
    ///
    /// # Errors
    ///
    /// Returns `E` on database failure.
    pub async fn nack(&self, id: Uuid, err: &str) -> Result<(), E> {
        let conn = self.db.conn()?;
        let db_backend = backend(&conn);
        let worker_id = self.worker_id;

        // First, read the current attempts count for this row.
        let attempts = read_attempts(&conn, db_backend, id, worker_id)
            .await
            .map_err(E::from)?;

        #[allow(clippy::cast_sign_loss)]
        if (attempts as u32) >= self.retry_cfg.max_attempts {
            // Dead-letter: transition to `dead`.
            let affected = dead_letter(&conn, db_backend, id, worker_id, err)
                .await
                .map_err(E::from)?;
            if affected == 0 {
                tracing::warn!(
                    namespace = %self.namespace,
                    id = %id,
                    "outbox dead-letter update affected 0 rows (lease may have expired)"
                );
            } else {
                tracing::error!(
                    namespace = %self.namespace,
                    id = %id,
                    attempts = attempts,
                    max_attempts = self.retry_cfg.max_attempts,
                    last_error = %err,
                    "outbox row dead-lettered after max delivery attempts"
                );
            }
        } else {
            // Schedule retry with backoff.
            let delay = compute_backoff_delay(attempts, &self.retry_cfg);
            let next_attempt = Utc::now() + delay;
            let affected = reschedule(&conn, db_backend, id, worker_id, err, next_attempt)
                .await
                .map_err(E::from)?;
            if affected == 0 {
                tracing::warn!(
                    namespace = %self.namespace,
                    id = %id,
                    "outbox reschedule update affected 0 rows (lease may have expired)"
                );
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Claim helpers (backend-specific)
// ---------------------------------------------------------------------------

/// Postgres claim: single UPDATE with `FOR UPDATE SKIP LOCKED` subquery + RETURNING.
#[allow(clippy::cast_possible_wrap)]
async fn claim_batch_pg(
    tx: &impl DBRunner,
    namespace: &str,
    worker_id: Uuid,
    locked_until: chrono::DateTime<Utc>,
    max_attempts: u32,
    batch_size: u32,
) -> crate::Result<Vec<ClaimedMessage>> {
    let sql = r"
        UPDATE modkit_outbox_events
        SET status = 'processing',
            attempts = attempts + 1,
            locked_by = $1,
            locked_until = $2,
            updated_at = NOW()
        WHERE id IN (
            SELECT id FROM modkit_outbox_events
            WHERE namespace = $3
              AND (
                (status = 'pending' AND next_attempt_at <= NOW())
                OR (status = 'processing' AND locked_until < NOW())
              )
              AND attempts < $4
            ORDER BY created_at ASC, id ASC
            LIMIT $5
            FOR UPDATE SKIP LOCKED
        )
        RETURNING id, namespace, topic, tenant_id, dedupe_key, payload::text, attempts
    ";

    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        sql,
        vec![
            Value::Uuid(Some(Box::new(worker_id))),
            Value::ChronoDateTimeUtc(Some(Box::new(locked_until))),
            Value::from(namespace.to_owned()),
            Value::from(max_attempts as i32),
            Value::from(i64::from(batch_size)),
        ],
    );

    let rows = query_all(tx, stmt).await?;
    rows.iter().map(parse_claimed_row_pg).collect()
}

/// `SQLite` claim: SELECT eligible IDs, then UPDATE, then SELECT back.
#[allow(clippy::cast_possible_wrap)]
async fn claim_batch_sqlite(
    tx: &impl DBRunner,
    namespace: &str,
    worker_id: Uuid,
    locked_until: chrono::DateTime<Utc>,
    max_attempts: u32,
    batch_size: u32,
) -> crate::Result<Vec<ClaimedMessage>> {
    let worker_id_str = worker_id.to_string();

    // Step 1: Find eligible row IDs.
    let select_sql = r"
        SELECT id FROM modkit_outbox_events
        WHERE namespace = $1
          AND (
            (status = 'pending' AND next_attempt_at <= strftime('%Y-%m-%d %H:%M:%f','now'))
            OR (status = 'processing' AND locked_until < strftime('%Y-%m-%d %H:%M:%f','now'))
          )
          AND attempts < $2
        ORDER BY created_at ASC, id ASC
        LIMIT $3
    ";

    let select_stmt = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        select_sql,
        vec![
            Value::from(namespace.to_owned()),
            Value::from(max_attempts as i32),
            Value::from(batch_size as i32),
        ],
    );

    let id_rows = query_all(tx, select_stmt).await?;
    if id_rows.is_empty() {
        return Ok(vec![]);
    }

    let ids: Vec<String> = id_rows
        .iter()
        .map(|r| {
            let id: String = r.try_get("", "id").map_err(DbError::Sea)?;
            Ok(id)
        })
        .collect::<crate::Result<Vec<_>>>()?;

    // Step 2: UPDATE those rows, re-checking eligibility predicates to guard
    // against concurrent claimers ($1=worker_id, $2=locked_until, $3=max_attempts, $4..$N=ids).
    let placeholders: Vec<String> = (0..ids.len()).map(|i| format!("${}", i + 4)).collect();
    let in_clause = placeholders.join(", ");
    let update_sql = format!(
        r"UPDATE modkit_outbox_events
           SET status = 'processing',
               attempts = attempts + 1,
               locked_by = $1,
               locked_until = $2,
               updated_at = strftime('%Y-%m-%d %H:%M:%f','now')
           WHERE id IN ({in_clause})
             AND (
               (status = 'pending' AND next_attempt_at <= strftime('%Y-%m-%d %H:%M:%f','now'))
               OR (status = 'processing' AND locked_until < strftime('%Y-%m-%d %H:%M:%f','now'))
             )
             AND attempts < $3",
    );

    let mut update_values: Vec<Value> = vec![
        Value::from(worker_id_str.clone()),
        Value::from(locked_until.format(SQLITE_TS_FMT).to_string()),
        Value::from(max_attempts as i32),
    ];
    for id in &ids {
        update_values.push(Value::from(id.clone()));
    }

    let update_stmt =
        Statement::from_sql_and_values(DatabaseBackend::Sqlite, &update_sql, update_values);
    exec(tx, update_stmt).await?;

    // Step 3: Read back claimed rows (guarded by locked_by so we only return
    // rows this worker actually claimed, not rows skipped by Step 2).
    let read_placeholders: Vec<String> = (0..ids.len()).map(|i| format!("${}", i + 2)).collect();
    let read_in_clause = read_placeholders.join(", ");
    let read_sql = format!(
        r"SELECT id, namespace, topic, tenant_id, dedupe_key, payload, attempts
           FROM modkit_outbox_events
           WHERE id IN ({read_in_clause})
             AND locked_by = $1
           ORDER BY created_at ASC, id ASC",
    );

    let mut read_values: Vec<Value> = vec![Value::from(worker_id_str)];
    for id in &ids {
        read_values.push(Value::from(id.clone()));
    }

    let read_stmt = Statement::from_sql_and_values(DatabaseBackend::Sqlite, &read_sql, read_values);
    let rows = query_all(tx, read_stmt).await?;
    rows.iter().map(parse_claimed_row_sqlite).collect()
}

// ---------------------------------------------------------------------------
// Nack helpers
// ---------------------------------------------------------------------------

/// Read the current `attempts` value for a row, guarded by `locked_by`.
async fn read_attempts(
    runner: &impl DBRunner,
    db_backend: DatabaseBackend,
    id: Uuid,
    worker_id: Uuid,
) -> crate::Result<i32> {
    let (sql, values) = match db_backend {
        DatabaseBackend::Postgres => (
            "SELECT attempts FROM modkit_outbox_events WHERE id = $1 AND locked_by = $2",
            vec![
                Value::Uuid(Some(Box::new(id))),
                Value::Uuid(Some(Box::new(worker_id))),
            ],
        ),
        DatabaseBackend::Sqlite => (
            "SELECT attempts FROM modkit_outbox_events WHERE id = $1 AND locked_by = $2",
            vec![
                Value::from(id.to_string()),
                Value::from(worker_id.to_string()),
            ],
        ),
        DatabaseBackend::MySql => {
            return Err(DbError::InvalidConfig("Unsupported backend".into()));
        }
    };

    let stmt = Statement::from_sql_and_values(db_backend, sql, values);
    let row = query_one(runner, stmt).await?.ok_or_else(|| {
        DbError::Other(anyhow::anyhow!(
            "outbox nack: row {id} is not leased by worker {worker_id}"
        ))
    })?;

    let attempts: i32 = row.try_get("", "attempts").map_err(DbError::Sea)?;
    Ok(attempts)
}

/// Transition a row to `dead` status. Returns the number of rows affected.
async fn dead_letter(
    runner: &impl DBRunner,
    db_backend: DatabaseBackend,
    id: Uuid,
    worker_id: Uuid,
    err_msg: &str,
) -> crate::Result<u64> {
    let (sql, values) = match db_backend {
        DatabaseBackend::Postgres => (
            r"UPDATE modkit_outbox_events
               SET status = 'dead',
                   last_error = $1,
                   locked_by = NULL,
                   locked_until = NULL,
                   updated_at = NOW()
               WHERE id = $2 AND locked_by = $3",
            vec![
                Value::from(err_msg.to_owned()),
                Value::Uuid(Some(Box::new(id))),
                Value::Uuid(Some(Box::new(worker_id))),
            ],
        ),
        DatabaseBackend::Sqlite => (
            r"UPDATE modkit_outbox_events
               SET status = 'dead',
                   last_error = $1,
                   locked_by = NULL,
                   locked_until = NULL,
                   updated_at = strftime('%Y-%m-%d %H:%M:%f','now')
               WHERE id = $2 AND locked_by = $3",
            vec![
                Value::from(err_msg.to_owned()),
                Value::from(id.to_string()),
                Value::from(worker_id.to_string()),
            ],
        ),
        DatabaseBackend::MySql => {
            return Err(DbError::InvalidConfig("Unsupported backend".into()));
        }
    };

    let stmt = Statement::from_sql_and_values(db_backend, sql, values);
    let result = exec(runner, stmt).await?;
    Ok(result.rows_affected())
}

/// Return a row to `pending` with a computed `next_attempt_at`. Returns the number of rows affected.
async fn reschedule(
    runner: &impl DBRunner,
    db_backend: DatabaseBackend,
    id: Uuid,
    worker_id: Uuid,
    err_msg: &str,
    next_attempt: chrono::DateTime<Utc>,
) -> crate::Result<u64> {
    let (sql, values) = match db_backend {
        DatabaseBackend::Postgres => (
            r"UPDATE modkit_outbox_events
               SET status = 'pending',
                   last_error = $1,
                   next_attempt_at = $2,
                   locked_by = NULL,
                   locked_until = NULL,
                   updated_at = NOW()
               WHERE id = $3 AND locked_by = $4",
            vec![
                Value::from(err_msg.to_owned()),
                Value::ChronoDateTimeUtc(Some(Box::new(next_attempt))),
                Value::Uuid(Some(Box::new(id))),
                Value::Uuid(Some(Box::new(worker_id))),
            ],
        ),
        DatabaseBackend::Sqlite => (
            r"UPDATE modkit_outbox_events
               SET status = 'pending',
                   last_error = $1,
                   next_attempt_at = $2,
                   locked_by = NULL,
                   locked_until = NULL,
                   updated_at = strftime('%Y-%m-%d %H:%M:%f','now')
               WHERE id = $3 AND locked_by = $4",
            vec![
                Value::from(err_msg.to_owned()),
                Value::from(next_attempt.format(SQLITE_TS_FMT).to_string()),
                Value::from(id.to_string()),
                Value::from(worker_id.to_string()),
            ],
        ),
        DatabaseBackend::MySql => {
            return Err(DbError::InvalidConfig("Unsupported backend".into()));
        }
    };

    let stmt = Statement::from_sql_and_values(db_backend, sql, values);
    let result = exec(runner, stmt).await?;
    Ok(result.rows_affected())
}

/// Compute exponential backoff delay with equal jitter.
///
/// `delay = min(base_delay * 2^(attempts - 1), max_delay)`
/// `jittered = uniform_random(delay / 2, delay)`
#[allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]
fn compute_backoff_delay(attempts: i32, cfg: &RetryCfg) -> chrono::Duration {
    let exp = (attempts - 1).max(0) as u32;
    let base_ms = cfg.base_delay.as_millis() as f64;
    let max_ms = cfg.max_delay.as_millis() as f64;

    let delay_ms = (base_ms * 2.0_f64.powi(exp as i32)).min(max_ms);
    let half = delay_ms / 2.0;

    let mut rng = rand::rng();
    let jittered_ms = rng.random_range(half..=delay_ms);

    chrono::Duration::milliseconds(jittered_ms as i64)
}

// ---------------------------------------------------------------------------
// Row parsing
// ---------------------------------------------------------------------------

fn parse_claimed_row_pg(row: &sea_orm::QueryResult) -> crate::Result<ClaimedMessage> {
    let id: Uuid = row.try_get("", "id").map_err(DbError::Sea)?;
    let namespace: String = row.try_get("", "namespace").map_err(DbError::Sea)?;
    let topic: String = row.try_get("", "topic").map_err(DbError::Sea)?;
    let tenant_id: Option<Uuid> = row.try_get("", "tenant_id").map_err(DbError::Sea)?;
    let dedupe_key: Option<String> = row.try_get("", "dedupe_key").map_err(DbError::Sea)?;
    let payload_str: String = row.try_get("", "payload").map_err(DbError::Sea)?;
    let attempts: i32 = row.try_get("", "attempts").map_err(DbError::Sea)?;

    let payload: serde_json::Value = serde_json::from_str(&payload_str)
        .map_err(|e| DbError::Other(anyhow::anyhow!("failed to parse outbox payload JSON: {e}")))?;

    Ok(ClaimedMessage {
        id,
        namespace,
        topic,
        tenant_id,
        dedupe_key,
        payload,
        attempts,
    })
}

fn parse_claimed_row_sqlite(row: &sea_orm::QueryResult) -> crate::Result<ClaimedMessage> {
    let id_str: String = row.try_get("", "id").map_err(DbError::Sea)?;
    let id: Uuid = id_str
        .parse()
        .map_err(|e| DbError::Other(anyhow::anyhow!("invalid UUID: {e}")))?;
    let namespace: String = row.try_get("", "namespace").map_err(DbError::Sea)?;
    let topic: String = row.try_get("", "topic").map_err(DbError::Sea)?;
    let tenant_id_str: Option<String> = row.try_get("", "tenant_id").map_err(DbError::Sea)?;
    let tenant_id = tenant_id_str
        .map(|s| s.parse::<Uuid>())
        .transpose()
        .map_err(|e| DbError::Other(anyhow::anyhow!("invalid tenant UUID: {e}")))?;
    let dedupe_key: Option<String> = row.try_get("", "dedupe_key").map_err(DbError::Sea)?;
    let payload_str: String = row.try_get("", "payload").map_err(DbError::Sea)?;
    let attempts: i32 = row.try_get("", "attempts").map_err(DbError::Sea)?;

    let payload: serde_json::Value = serde_json::from_str(&payload_str)
        .map_err(|e| DbError::Other(anyhow::anyhow!("failed to parse outbox payload JSON: {e}")))?;

    Ok(ClaimedMessage {
        id,
        namespace,
        topic,
        tenant_id,
        dedupe_key,
        payload,
        attempts,
    })
}
