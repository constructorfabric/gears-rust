//! Transactional outbox enqueue function.
//!
//! The `enqueue` function inserts an outbox row inside the caller's existing
//! transaction, ensuring atomic persistence with the domain side effects.

use sea_orm::{DatabaseBackend, Statement, Value};
use uuid::Uuid;

use super::helpers::{backend, exec, query_one};
use super::types::OutboxMessage;
use crate::DbError;
use crate::secure::DBRunner;

/// Enqueue an outbox message inside the caller's database transaction.
///
/// The row is inserted into `modkit_outbox_events` with `status = 'pending'`.
/// If a `dedupe_key` is provided and a matching row already exists, the insert
/// is silently skipped (idempotent enqueue) and the existing row's ID is returned.
///
/// `dedupe_key` is an opaque, producer-defined string. The store enforces
/// uniqueness via a partial unique index on `(namespace, topic, dedupe_key)`;
/// no format validation is performed by this function.
///
/// # Errors
///
/// Returns `DbError` if:
/// - The database insert fails
/// - The backend is not supported
pub async fn enqueue(runner: &impl DBRunner, msg: OutboxMessage) -> crate::Result<Uuid> {
    let id = Uuid::new_v4();
    let db_backend = backend(runner);
    let payload_str = serde_json::to_string(&msg.payload)
        .map_err(|e| DbError::Other(anyhow::anyhow!("failed to serialize outbox payload: {e}")))?;

    let (sql, values) = match db_backend {
        DatabaseBackend::Postgres => (
            r"INSERT INTO modkit_outbox_events
                (id, namespace, topic, tenant_id, dedupe_key, payload, status, attempts, next_attempt_at, created_at, updated_at)
               VALUES
                ($1, $2, $3, $4, $5, $6::jsonb, 'pending', 0, NOW(), NOW(), NOW())
               ON CONFLICT (namespace, topic, dedupe_key) WHERE dedupe_key IS NOT NULL
               DO NOTHING"
                .to_owned(),
            vec![
                Value::Uuid(Some(Box::new(id))),
                Value::from(msg.namespace.to_owned()),
                Value::from(msg.topic.to_owned()),
                Value::Uuid(msg.tenant_id.map(Box::new)),
                Value::String(msg.dedupe_key.clone().map(Box::new)),
                Value::from(payload_str.clone()),
            ],
        ),
        DatabaseBackend::Sqlite => (
            r"INSERT OR IGNORE INTO modkit_outbox_events
                (id, namespace, topic, tenant_id, dedupe_key, payload, status, attempts, next_attempt_at, created_at, updated_at)
               VALUES
                ($1, $2, $3, $4, $5, $6, 'pending', 0, strftime('%Y-%m-%d %H:%M:%f','now'), strftime('%Y-%m-%d %H:%M:%f','now'), strftime('%Y-%m-%d %H:%M:%f','now'))"
                .to_owned(),
            vec![
                Value::from(id.to_string()),
                Value::from(msg.namespace.to_owned()),
                Value::from(msg.topic.to_owned()),
                Value::String(msg.tenant_id.map(|u| Box::new(u.to_string()))),
                Value::String(msg.dedupe_key.clone().map(Box::new)),
                Value::from(payload_str.clone()),
            ],
        ),
        DatabaseBackend::MySql => {
            return Err(DbError::InvalidConfig(
                "Outbox enqueue is not supported for this database backend".into(),
            ));
        }
    };

    let stmt = Statement::from_sql_and_values(db_backend, &sql, values);
    let result = exec(runner, stmt).await?;

    if result.rows_affected() == 0 {
        // Dedupe conflict — look up the existing row's ID.
        if let Some(ref dk) = msg.dedupe_key {
            let existing_id =
                lookup_existing_id(runner, db_backend, msg.namespace, msg.topic, dk).await?;
            return Ok(existing_id);
        }
        // No dedupe_key but 0 rows affected — unexpected.
        return Err(DbError::Other(anyhow::anyhow!(
            "outbox enqueue inserted 0 rows without a dedupe_key conflict"
        )));
    }

    Ok(id)
}

/// Look up the ID of an existing outbox row by `(namespace, topic, dedupe_key)`.
async fn lookup_existing_id(
    runner: &impl DBRunner,
    db_backend: DatabaseBackend,
    namespace: &str,
    topic: &str,
    dedupe_key: &str,
) -> crate::Result<Uuid> {
    let sql = match db_backend {
        DatabaseBackend::Postgres | DatabaseBackend::Sqlite => {
            r"SELECT id FROM modkit_outbox_events WHERE namespace = $1 AND topic = $2 AND dedupe_key = $3"
        }
        DatabaseBackend::MySql => {
            return Err(DbError::InvalidConfig(
                "Outbox lookup is not supported for this database backend".into(),
            ));
        }
    };

    let stmt = Statement::from_sql_and_values(
        db_backend,
        sql,
        vec![
            Value::from(namespace.to_owned()),
            Value::from(topic.to_owned()),
            Value::from(dedupe_key.to_owned()),
        ],
    );

    let row = query_one(runner, stmt).await?.ok_or_else(|| {
        DbError::Other(anyhow::anyhow!(
            "outbox dedupe conflict but existing row not found for key: {dedupe_key}"
        ))
    })?;

    parse_uuid_from_row(&row, db_backend)
}

/// Parse a UUID `id` column from a query result, handling backend differences.
fn parse_uuid_from_row(
    row: &sea_orm::QueryResult,
    db_backend: DatabaseBackend,
) -> crate::Result<Uuid> {
    if db_backend == DatabaseBackend::Postgres {
        row.try_get::<Uuid>("", "id").map_err(DbError::Sea)
    } else {
        // SQLite stores UUIDs as text.
        let id_str: String = row.try_get("", "id").map_err(DbError::Sea)?;
        id_str
            .parse::<Uuid>()
            .map_err(|e| DbError::Other(anyhow::anyhow!("invalid UUID in outbox row: {e}")))
    }
}
