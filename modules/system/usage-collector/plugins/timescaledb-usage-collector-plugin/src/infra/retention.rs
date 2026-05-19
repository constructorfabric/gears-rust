//! Retention policy setup for the `TimescaleDB` storage plugin.

use std::time::Duration;

use sqlx::PgPool;

use crate::domain::error::StoragePluginError;
use crate::infra::db_error::DbError;

/// Applies the `TimescaleDB` retention policy to `usage_records` and prunes
/// expired rows from `usage_idempotency_keys`.
///
/// The `usage_records` retention policy is installed as a `TimescaleDB` job and
/// runs continuously. The `usage_idempotency_keys` table has no equivalent
/// hypertable retention policy because it is a plain table, so its cleanup is
/// driven separately — once here at startup, then periodically by the plugin's
/// background cleanup task (see [`cleanup_idempotency_keys`]).
///
/// `idempotency_retention` is intentionally distinct from `retention`: the
/// hypertable retention window can be measured in months or years, while
/// idempotency dedup windows only need to cover client retry horizons (hours
/// to a few days). Coupling them would grow `usage_idempotency_keys` linearly
/// with ingest volume and slow the periodic `DELETE`.
///
/// # Errors
///
/// Returns [`StoragePluginError`] if any statement fails.
pub async fn setup_retention_policy(
    pool: &PgPool,
    retention: Duration,
    idempotency_retention: Duration,
) -> Result<(), StoragePluginError> {
    let interval = format!("{} seconds", retention.as_secs());

    let retention_err = |context: &'static str| {
        move |e: sqlx::Error| StoragePluginError::RetentionPolicySetupFailed {
            context: context.to_owned(),
            source: DbError::boxed(e),
        }
    };

    // Reconcile the existing retention policy with the configured duration.
    //
    // `add_retention_policy(..., if_not_exists => true)` is a no-op when a
    // policy is already registered — so an operator who restarts with a new
    // `retention_default` would silently keep running the old policy. Query
    // the existing job, compare its `drop_after` against the configured value
    // (compared in `INTERVAL` space so `"3600 seconds"` matches `"01:00:00"`),
    // and reinstall the policy only when it actually differs.
    let existing: Option<(i32, Option<String>)> = sqlx::query_as(
        "SELECT job_id, config->>'drop_after' \
         FROM timescaledb_information.jobs \
         WHERE proc_name = 'policy_retention' \
           AND hypertable_name = 'usage_records' \
         LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(retention_err("failed to query existing retention policy"))?;

    match existing {
        Some((job_id, Some(current))) => {
            let matches_config: bool = sqlx::query_scalar("SELECT $1::interval = $2::interval")
                .bind(&current)
                .bind(&interval)
                .fetch_one(pool)
                .await
                .map_err(retention_err(
                    "failed to compare existing retention interval with configured value",
                ))?;
            if !matches_config {
                let mut tx = pool.begin().await.map_err(retention_err(
                    "failed to begin retention reconciliation transaction",
                ))?;
                sqlx::query("SELECT delete_job($1)")
                    .bind(job_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(retention_err("failed to delete stale retention policy job"))?;
                sqlx::query("SELECT add_retention_policy('usage_records', $1::interval)")
                    .bind(&interval)
                    .execute(&mut *tx)
                    .await
                    .map_err(retention_err(
                        "failed to install reconciled retention policy",
                    ))?;
                tx.commit()
                    .await
                    .map_err(retention_err("failed to commit retention reconciliation"))?;
            }
        }
        _ => {
            sqlx::query(
                "SELECT add_retention_policy('usage_records', $1::interval, if_not_exists => true)",
            )
            .bind(&interval)
            .execute(pool)
            .await
            .map_err(retention_err(
                "failed to add retention policy for usage_records",
            ))?;
        }
    }

    cleanup_idempotency_keys(pool, idempotency_retention).await?;

    Ok(())
}

/// Deletes rows from `usage_idempotency_keys` older than `retention`.
///
/// Intended to be invoked both at startup (from [`setup_retention_policy`]) and
/// on a recurring schedule by the plugin's background cleanup task, so the
/// table stays bounded on long-running processes.
///
/// # Errors
///
/// Returns [`StoragePluginError::RetentionPolicySetupFailed`] if the statement fails.
pub async fn cleanup_idempotency_keys(
    pool: &PgPool,
    retention: Duration,
) -> Result<u64, StoragePluginError> {
    let interval = format!("{} seconds", retention.as_secs());
    let rows =
        sqlx::query("DELETE FROM usage_idempotency_keys WHERE created_at < NOW() - $1::interval")
            .bind(&interval)
            .execute(pool)
            .await
            .map_err(|e| StoragePluginError::RetentionPolicySetupFailed {
                context: "failed to clean up expired idempotency keys".to_owned(),
                source: DbError::boxed(e),
            })?
            .rows_affected();
    Ok(rows)
}
