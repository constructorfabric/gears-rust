//! `PostgreSQL` implementation of the insert port.

use async_trait::async_trait;
use sqlx::PgPool;
use usage_collector_sdk::models::{UsageKind, UsageRecord};

use crate::domain::error::StoragePluginError;
use crate::domain::insert_port::InsertPort;
use crate::infra::db_error::DbError;
use crate::infra::is_transient_pg_error;

/// Whether the record carries a non-blank idempotency key.
///
/// Mirrors the trim/empty discipline applied at `domain::client` so the
/// idempotency-claim transaction is only taken for keys that the domain
/// layer would also consider present. A future caller bypassing the domain
/// client (e.g. an integration test or a new ingest path) cannot smuggle a
/// whitespace-only key into the claim table through this port.
fn idempotency_key_is_present(record: &UsageRecord) -> bool {
    !record.idempotency_key.trim().is_empty()
}

fn classify_insert_error(e: sqlx::Error) -> StoragePluginError {
    if is_transient_pg_error(&e) {
        return StoragePluginError::Transient(DbError::boxed(e));
    }
    if let sqlx::Error::Database(ref db_err) = e
        && db_err.code().as_deref() == Some("23505")
    {
        return StoragePluginError::UnexpectedUniqueViolation(DbError::boxed(e));
    }
    StoragePluginError::QueryFailed(DbError::boxed(e))
}

pub struct PgInsertPort {
    pool: PgPool,
}

impl PgInsertPort {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const INSERT_RECORD_SQL: &str = "INSERT INTO usage_records (
        tenant_id, module, kind, metric, value, timestamp, idempotency_key,
        resource_id, resource_type, subject_id, subject_type, metadata, ingested_at
    )
    VALUES (
        $1, $2, $3, $4, $5::numeric, $6, NULLIF($7, ''),
        $8, $9, $10, $11, $12, NOW()
    )";

#[async_trait]
impl InsertPort for PgInsertPort {
    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-6
    // ingested_at is set via NOW() in the INSERT SQL — not populated from the caller
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-6
    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-3
    async fn insert_usage_record(&self, record: &UsageRecord) -> Result<u64, StoragePluginError> {
        let kind_str = match record.kind {
            UsageKind::Counter => "counter",
            UsageKind::Gauge => "gauge",
        };
        let metadata_json = record.metadata.as_ref();
        let subject_id = record.subject.as_ref().map(|s| s.id);
        let subject_type = record.subject.as_ref().and_then(|s| s.r#type.clone());

        // TimescaleDB unique indexes must include the partition column (timestamp), so
        // cross-partition idempotency is enforced via a separate plain table inside a
        // transaction instead of an ON CONFLICT clause on usage_records.
        if record.kind == UsageKind::Counter && idempotency_key_is_present(record) {
            let mut tx = self.pool.begin().await.map_err(classify_insert_error)?;

            let claimed = sqlx::query(
                "INSERT INTO usage_idempotency_keys (tenant_id, idempotency_key) \
                 VALUES ($1, $2) \
                 ON CONFLICT (tenant_id, idempotency_key) DO NOTHING",
            )
            .bind(record.tenant_id)
            .bind(&record.idempotency_key)
            .execute(&mut *tx)
            .await
            .map_err(classify_insert_error)?
            .rows_affected();

            if claimed == 0 {
                if let Err(e) = tx.rollback().await {
                    // Rollback failure on the dedup path leaves the connection's
                    // transaction state ambiguous: the idempotency-key claim may
                    // have committed or may still be open. Returning `Ok(0)`
                    // (the normal dedup signal) would let the caller treat this
                    // as a successful dedup while the next caller on the same
                    // connection inherits the ambiguous state. Surfacing it as
                    // `Transient` instead drops the transaction (sqlx marks the
                    // connection broken so it is not returned to the pool) and
                    // tells the caller the operation can be safely retried —
                    // the next attempt will hit the existing claim (if it
                    // committed) and dedup correctly, or re-claim cleanly (if
                    // the rollback eventually took effect).
                    //
                    // `idempotency_key` is caller-supplied and frequently
                    // carries request IDs or other user-controllable values,
                    // so it is deliberately excluded from the structured
                    // fields to bound log-index cardinality and avoid
                    // surfacing PII-adjacent identifiers on this hot path.
                    tracing::warn!(
                        error = %e,
                        tenant_id = %record.tenant_id,
                        module = %record.module,
                        "rollback failed after idempotency key conflict; surfacing as transient"
                    );
                    return Err(StoragePluginError::Transient(DbError::boxed(e)));
                }
                return Ok(0);
            }

            let rows = sqlx::query(INSERT_RECORD_SQL)
                .bind(record.tenant_id)
                .bind(&record.module)
                .bind(kind_str)
                .bind(&record.metric)
                .bind(record.value)
                .bind(record.timestamp)
                .bind(&record.idempotency_key)
                .bind(record.resource_id)
                .bind(&record.resource_type)
                .bind(subject_id)
                .bind(&subject_type)
                .bind(metadata_json)
                .execute(&mut *tx)
                .await
                .map_err(classify_insert_error)?
                .rows_affected();

            tx.commit().await.map_err(classify_insert_error)?;
            return Ok(rows);
        }

        let result = sqlx::query(INSERT_RECORD_SQL)
            .bind(record.tenant_id)
            .bind(&record.module)
            .bind(kind_str)
            .bind(&record.metric)
            .bind(record.value)
            .bind(record.timestamp)
            .bind(&record.idempotency_key)
            .bind(record.resource_id)
            .bind(&record.resource_type)
            .bind(subject_id)
            .bind(&subject_type)
            .bind(metadata_json)
            .execute(&self.pool)
            .await
            .map_err(classify_insert_error)?;

        Ok(result.rows_affected())
    }
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-create-usage-record:p1:inst-cur-3
}
