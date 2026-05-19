//! Continuous aggregate setup for the `TimescaleDB` storage plugin.

use sqlx::PgPool;

use crate::domain::error::MigrationError;
use crate::infra::db_error::DbError;

/// Name of the 1-hour continuous aggregate view this plugin creates and reads.
///
/// Centralised here so `pg_query_port` and any future readers can address the
/// view by symbolic name rather than re-typing a string literal (a mismatch
/// would silently route reads to a non-existent view).
pub const CONTINUOUS_AGGREGATE_VIEW: &str = "usage_agg_1h";

// @cpt-algo:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1
/// # Errors
///
/// Returns [`MigrationError`] if any DDL or policy registration statement fails.
pub async fn setup_continuous_aggregate(pool: &PgPool) -> Result<(), MigrationError> {
    fn cagg_err(context: &str) -> impl Fn(sqlx::Error) -> MigrationError + '_ {
        move |source| MigrationError::ContinuousAggregateSetupFailed {
            context: context.to_owned(),
            source: Some(DbError::boxed(source)),
        }
    }

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-1
    let view_existed: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM timescaledb_information.continuous_aggregates
            WHERE view_name = 'usage_agg_1h'
        )",
    )
    .fetch_one(pool)
    .await
    .map_err(cagg_err("failed to check if usage_agg_1h exists"))?;

    if !view_existed {
        sqlx::query(
            "CREATE MATERIALIZED VIEW IF NOT EXISTS usage_agg_1h \
             WITH (timescaledb.continuous) AS \
             SELECT \
                 time_bucket('1 hour', timestamp) AS bucket, \
                 tenant_id, \
                 metric, \
                 module, \
                 resource_type, \
                 subject_type, \
                 SUM(value)  AS sum_val, \
                 COUNT(*)    AS cnt_val, \
                 MIN(value)  AS min_val, \
                 MAX(value)  AS max_val \
             FROM usage_records \
             GROUP BY bucket, tenant_id, metric, module, resource_type, subject_type \
             WITH NO DATA",
        )
        .execute(pool)
        .await
        .map_err(cagg_err(
            "failed to create usage_agg_1h continuous aggregate view",
        ))?;
    }
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-1

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-2
    // Capture the returned job id from `add_continuous_aggregate_policy`
    // instead of `.execute`-ing it. A non-NULL, positive job id is the
    // function's documented success signal — whether the policy is created
    // here or already exists under `if_not_exists => true`, the call returns
    // the existing job id. This avoids relying on the internal `proc_name`
    // symbol (`policy_refresh_continuous_aggregate`) during post-setup
    // verification, which changes between TimescaleDB majors and would turn
    // an upgrade into a hard startup failure for a healthy plugin.
    let policy_job_id: Option<i32> = sqlx::query_scalar(
        "SELECT add_continuous_aggregate_policy( \
             'usage_agg_1h', \
             start_offset      => INTERVAL '3 hours', \
             end_offset        => INTERVAL '1 hour', \
             schedule_interval => INTERVAL '30 minutes', \
             if_not_exists     => true \
         )",
    )
    .fetch_one(pool)
    .await
    .map_err(cagg_err(
        "failed to register refresh policy for usage_agg_1h",
    ))?;
    let job_id = policy_job_id.ok_or_else(|| MigrationError::ContinuousAggregateSetupFailed {
        context: "add_continuous_aggregate_policy returned NULL job id".to_owned(),
        source: None,
    })?;
    if job_id <= 0 {
        return Err(MigrationError::ContinuousAggregateSetupFailed {
            context: format!(
                "add_continuous_aggregate_policy returned non-positive job id: {job_id}"
            ),
            source: None,
        });
    }
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-2

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-3
    if !view_existed {
        sqlx::query(
            "CALL refresh_continuous_aggregate('usage_agg_1h', NULL, now() - INTERVAL '1 hour')",
        )
        .execute(pool)
        .await
        .map_err(cagg_err(
            "failed to trigger initial refresh of usage_agg_1h",
        ))?;
    }
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-3

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-4
    // View verification reads the public `continuous_aggregates` information
    // view — its shape is part of TimescaleDB's documented API. The refresh
    // policy was already verified above via the job id captured from
    // `add_continuous_aggregate_policy`, so we do not re-query
    // `timescaledb_information.jobs` here (the previous `proc_name` filter
    // relied on an internal symbol that is not a stable contract across
    // TimescaleDB majors).
    let view_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM timescaledb_information.continuous_aggregates
            WHERE view_name = 'usage_agg_1h'
        )",
    )
    .fetch_one(pool)
    .await
    .map_err(cagg_err("failed to verify usage_agg_1h view exists"))?;

    if !view_exists {
        return Err(MigrationError::ContinuousAggregateSetupFailed {
            context: "post-setup verification failed: usage_agg_1h view not found".to_owned(),
            source: None,
        });
    }
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-4

    // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-5
    Ok(())
    // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-continuous-aggregate:p1:inst-cagg-5
}
