//! `PostgreSQL` implementation of the query port.

use async_trait::async_trait;
use chrono::{DateTime, Timelike, Utc};
use sqlx::PgPool;
use sqlx::Row as _;
use sqlx::postgres::{PgArguments, PgRow};
use usage_collector_sdk::cursor_filter::{
    RawQueryFilters, raw_query_effective_order, raw_query_filter_hash,
};
use usage_collector_sdk::models::{
    AggregationFn, AggregationQuery, AggregationResult, BucketSize, GroupByDimension, RawQuery,
    Subject, UsageKind, UsageRecord,
};
use usage_collector_sdk::{
    CursorV1, Page, PageInfo, SortDir, UsageCollectorError, UsageRecordError,
};
use uuid::Uuid;

use crate::domain::error::{ScopeTranslationError, StoragePluginError};
use crate::domain::query_port::QueryPort;
use crate::domain::scope::{SqlValue, scope_constrains_record_ids, scope_to_sql};
use crate::infra::continuous_aggregate::CONTINUOUS_AGGREGATE_VIEW;
use crate::infra::db_error::DbError;
use crate::infra::is_transient_pg_error;

/// Routes a `sqlx::Error` through [`StoragePluginError`] so the SQLSTATE and
/// `source()` chain are preserved in the rendered [`UsageCollectorError`]
/// detail. Without this round-trip the bare `format!("storage error: {e}")`
/// loses the database error code at the API boundary.
fn classify_query_error(e: sqlx::Error) -> UsageCollectorError {
    if is_transient_pg_error(&e) {
        StoragePluginError::Transient(DbError::boxed(e)).into()
    } else {
        StoragePluginError::QueryFailed(DbError::boxed(e)).into()
    }
}

fn map_scope_error(err: &ScopeTranslationError) -> UsageCollectorError {
    // Both variants collapse into the same public `PermissionDenied` reason so
    // the API boundary does not leak which authorization predicate failed.
    // Operators triaging a 403 still need to distinguish "PDP returned empty
    // scope (fail-closed)" from "PDP emitted an InGroup/InGroupSubtree
    // predicate this plugin cannot translate" from "PDP used an unknown
    // property name" — three very different bugs (auth misconfig vs feature
    // gap vs name drift). Emit a structured warn here so the diagnostic
    // survives in logs even though the public error carries only the stable
    // reason code.
    tracing::warn!(
        reason = ?err,
        "scope translation rejected query; returning PermissionDenied"
    );
    match err {
        ScopeTranslationError::EmptyScope | ScopeTranslationError::UnsupportedPredicate { .. } => {
            UsageRecordError::permission_denied()
                .with_reason("AUTHORIZATION_DENIED")
                .create()
        }
    }
}

/// Maximum number of rows `query_aggregated` may return.
///
/// Sourced as a plugin-side constant because the HEAD SDK's `AggregationQuery`
/// no longer carries `max_rows`; the gateway-configured `MAX_AGG_ROWS` is the
/// platform-recommended ceiling (see `usage_collector_sdk::plugin_api`
/// docstring on `query_aggregated`). Phase 6 may move this into plugin
/// configuration without changing the byte-identical resource-exhausted
/// detail string.
const MAX_AGG_ROWS: usize = 10_000;

/// Maximum number of rows `query_raw` may return per page.
///
/// Mirrors the symmetric ceiling already applied to `query_aggregated` via
/// `MAX_AGG_ROWS`. Without this bound a caller can request `u32::MAX` rows;
/// the resulting `LIMIT page_size + 1` is a full-scan ceiling the database
/// will happily evaluate.
const MAX_RAW_PAGE_SIZE: u32 = 10_000;

pub struct PgQueryPort {
    pool: PgPool,
    max_agg_rows: usize,
}

impl PgQueryPort {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            max_agg_rows: MAX_AGG_ROWS,
        }
    }

    /// Test-only constructor that overrides the aggregation row cap.
    ///
    /// Production code uses [`PgQueryPort::new`], which binds the cap to the
    /// platform-recommended [`MAX_AGG_ROWS`]. Integration tests use this
    /// constructor with a small cap so the cost-control invariant can be
    /// exercised without emitting > 10 000 rows.
    #[cfg(feature = "integration")]
    #[must_use]
    pub fn new_with_max_agg_rows(pool: PgPool, max_agg_rows: usize) -> Self {
        Self { pool, max_agg_rows }
    }
}

fn add_arg<T>(args: &mut PgArguments, value: T) -> Result<(), UsageCollectorError>
where
    T: for<'q> sqlx::Encode<'q, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    use sqlx::Arguments as _;
    args.add(value).map_err(|e| {
        StoragePluginError::Serialization {
            context: "SQL argument binding failed".to_owned(),
            source: e,
        }
        .into()
    })
}

/// Decodes column `col` from `row` as `T`, mapping `sqlx::Error` to a uniform
/// `UsageCollectorError::Internal` carrying the column name. Centralising this
/// keeps the row-decode sites in `query_aggregated` / `query_raw` to one line
/// each and ensures every decode error surfaces the same wording.
fn decode<'r, T>(row: &'r PgRow, col: &str) -> Result<T, UsageCollectorError>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get::<T, _>(col).map_err(|e| {
        StoragePluginError::Serialization {
            context: format!("row decode error ({col})"),
            source: DbError::boxed(e),
        }
        .into()
    })
}

fn raw_agg_expr(func: AggregationFn) -> &'static str {
    match func {
        AggregationFn::Sum => "SUM(value)::float8 AS agg_value",
        AggregationFn::Count => "COUNT(*)::float8 AS agg_value",
        AggregationFn::Min => "MIN(value)::float8 AS agg_value",
        AggregationFn::Max => "MAX(value)::float8 AS agg_value",
        AggregationFn::Avg => "AVG(value)::float8 AS agg_value",
    }
}

fn cagg_agg_expr(func: AggregationFn) -> &'static str {
    match func {
        AggregationFn::Sum => "SUM(sum_val)::float8 AS agg_value",
        AggregationFn::Count => "SUM(cnt_val)::float8 AS agg_value",
        AggregationFn::Min => "MIN(min_val)::float8 AS agg_value",
        AggregationFn::Max => "MAX(max_val)::float8 AS agg_value",
        AggregationFn::Avg => "(SUM(sum_val) / NULLIF(SUM(cnt_val), 0))::float8 AS agg_value",
    }
}

fn bucket_size_to_pg_interval(size: BucketSize) -> &'static str {
    match size {
        BucketSize::Minute => "1 minute",
        BucketSize::Hour => "1 hour",
        BucketSize::Day => "1 day",
        BucketSize::Week => "1 week",
        BucketSize::Month => "1 month",
    }
}

/// Per-path knobs for building an aggregation SELECT.
///
/// The raw-hypertable path and the continuous-aggregate path differ only in the
/// table, time column, aggregation expression, and whether the row-level
/// `resource_id` / `subject_id` columns are visible (the CAGG groups them out).
/// Centralising those knobs lets a single SQL assembler serve both branches.
struct AggSqlVariant {
    /// Table / view name to SELECT from.
    table: &'static str,
    /// Time column on the table/view (`timestamp` for raw, `bucket` for the CAGG).
    time_col: &'static str,
    /// Per-function aggregation expression, including the `AS agg_value` alias.
    agg_expr: &'static str,
    /// Whether `resource_id` / `subject_id` are visible (only on the raw path).
    /// Controls both SELECT/GROUP BY projection and WHERE-clause id filters.
    has_id_columns: bool,
    /// Upper-bound comparator for the time-range filter.
    ///
    /// The raw path uses `<=` (closed interval over individual record timestamps).
    /// The CAGG path uses `<` so the bucket at `end` — which covers
    /// `[end, end + 1h)` and therefore extends past the requested range — is
    /// excluded; a raw-record boundary correction (see `boundary_correction`)
    /// then UNIONs in records at exactly `timestamp = end`, restoring the
    /// closed-interval contract the spec requires.
    end_op: &'static str,
    /// Whether to UNION the bucket-aligned aggregate with a raw boundary
    /// projection. Only the CAGG path sets this — the raw path is already
    /// closed-interval through its `<=` comparator.
    boundary_correction: bool,
}

/// Predicate: is the requested time range safely served by the 1-hour CAGG?
///
/// The CAGG path is only correct when:
/// * both endpoints are aligned to hour boundaries — otherwise the bucket at
///   each endpoint contains records outside the requested range and the result
///   is under- or over-counted;
/// * the end is no later than the materialized horizon (`now - 1 hour`,
///   matching the refresh policy `end_offset`) — otherwise the unrefreshed
///   tail of the request is silently empty under
///   `timescaledb.materialized_only = true` (the `TimescaleDB` ≥ 2.13 default).
///
/// When either condition fails the caller must route to the raw hypertable.
///
/// `now` is injected (rather than read inline via `Utc::now()`) so the
/// materialized-horizon edge is unit-testable without race-prone
/// `Utc::now() ± Duration::hours(N)` workarounds. Production call sites pass
/// `Utc::now()`; tests pin it.
fn cagg_safe_range(start: DateTime<Utc>, end: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    fn hour_aligned(t: DateTime<Utc>) -> bool {
        t.minute() == 0 && t.second() == 0 && t.nanosecond() == 0
    }
    let materialized_horizon = now - chrono::Duration::hours(1);
    hour_aligned(start) && hour_aligned(end) && end <= materialized_horizon
}

/// Pre-bound positional parameter indices the caller has already added to `args`
/// for the inclusive time-range filter.
#[derive(Copy, Clone)]
struct TimeRangeArgs {
    start_idx: usize,
    end_idx: usize,
}

/// Rejects the case where the caller supplied both `bucket_size` and a
/// `GroupByDimension::TimeBucket(bs)` but the two carry different `BucketSize`s.
/// Extracted so this contract can be unit-tested without a `PgPool`.
fn validate_bucket_size_consistency(
    bucket_size: Option<BucketSize>,
    group_by: &[GroupByDimension],
) -> Result<(), UsageCollectorError> {
    let Some(requested) = bucket_size else {
        return Ok(());
    };
    for d in group_by {
        if let GroupByDimension::TimeBucket(group_bs) = d
            && *group_bs != requested
        {
            return Err(UsageRecordError::invalid_argument()
                .with_constraint(
                    "bucket_size must match group_by TimeBucket size when both are set",
                )
                .create());
        }
    }
    Ok(())
}

/// Builds the `FROM` sub-query that UNIONs the bucket-aligned CAGG aggregate
/// with the boundary records from the raw hypertable.
///
/// The CAGG view groups raw records into half-open hour intervals
/// (`[bucket, bucket + 1h)`). Using `bucket <= $end` on a closed-interval
/// request would include all of `[end, end + 1h)` — a full extra bucket beyond
/// the requested range. Using `bucket < $end` is bucket-correct but silently
/// excludes records at exactly `timestamp = end`, which the raw path WOULD
/// include (its `timestamp <= $end` predicate is inclusive).
///
/// Restore the closed-interval contract by UNION ALL'ing the bucket-aligned
/// aggregate with a raw-table projection of the boundary records, casting each
/// raw row into pre-aggregated shape so the outer SUM/MIN/MAX over
/// `sum_val`/`cnt_val`/`min_val`/`max_val` merges them transparently. The
/// routing guard `cagg_safe_range` already keeps both endpoints hour-aligned,
/// so `time_bucket('1 hour', $end)` collapses to `$end` and the boundary rows
/// land in the correct bucket key.
///
/// The CAGG path is only taken when `resource_id` and `subject_id` are absent
/// from both filters and scope (see `use_raw_path` upstream), so every clause
/// in `filter_sql` references columns present on both `usage_agg_1h` and
/// `usage_records`.
fn build_cagg_boundary_corrected_from(
    table: &str,
    time_col: &str,
    filter_sql: &str,
    time_start_idx: usize,
    time_end_idx: usize,
) -> String {
    format!(
        "(SELECT bucket, metric, module, resource_type, subject_type, \
              sum_val, cnt_val, min_val, max_val \
          FROM {table} \
          WHERE {filter_sql} AND {time_col} >= ${time_start_idx} \
              AND {time_col} < ${time_end_idx} \
          UNION ALL \
          SELECT time_bucket('1 hour', timestamp) AS bucket, metric, module, \
              resource_type, subject_type, \
              value AS sum_val, 1::bigint AS cnt_val, value AS min_val, \
              value AS max_val \
          FROM usage_records \
          WHERE {filter_sql} AND timestamp = ${time_end_idx}) AS unified"
    )
}

fn build_aggregation_sql(
    variant: &AggSqlVariant,
    query: &AggregationQuery,
    scope_sql: &str,
    args: &mut PgArguments,
    param_idx: &mut usize,
    time_range: TimeRangeArgs,
    max_agg_rows: usize,
) -> Result<String, UsageCollectorError> {
    let AggSqlVariant {
        table,
        time_col,
        agg_expr,
        has_id_columns,
        end_op,
        boundary_correction,
    } = *variant;
    let TimeRangeArgs {
        start_idx: time_start_idx,
        end_idx: time_end_idx,
    } = time_range;
    let time_bucket_size = query.group_by.iter().find_map(|d| match d {
        GroupByDimension::TimeBucket(bs) => Some(*bs),
        _ => None,
    });
    let has_time_bucket = time_bucket_size.is_some();
    let has_usage_type = query.group_by.contains(&GroupByDimension::UsageType);
    let has_subject = query.group_by.contains(&GroupByDimension::Subject);
    let has_resource = query.group_by.contains(&GroupByDimension::Resource);
    let has_source = query.group_by.contains(&GroupByDimension::Source);

    let mut select_cols: Vec<String> = Vec::new();
    let mut group_by_exprs: Vec<String> = Vec::new();

    if let Some(bs) = time_bucket_size {
        let interval = bucket_size_to_pg_interval(bs);
        select_cols.push(format!(
            "time_bucket('{interval}', {time_col}) AS bucket_start"
        ));
        group_by_exprs.push(format!("time_bucket('{interval}', {time_col})"));
    }
    if has_usage_type {
        select_cols.push("metric AS usage_type".to_owned());
        group_by_exprs.push("metric".to_owned());
    }
    if has_subject {
        if has_id_columns {
            select_cols.push("subject_id".to_owned());
            group_by_exprs.push("subject_id".to_owned());
        }
        select_cols.push("subject_type".to_owned());
        group_by_exprs.push("subject_type".to_owned());
    }
    if has_resource {
        if has_id_columns {
            select_cols.push("resource_id".to_owned());
            group_by_exprs.push("resource_id".to_owned());
        }
        select_cols.push("resource_type".to_owned());
        group_by_exprs.push("resource_type".to_owned());
    }
    if has_source {
        select_cols.push("module AS source".to_owned());
        group_by_exprs.push("module".to_owned());
    }
    select_cols.push(agg_expr.to_owned());

    let select_clause = select_cols.join(", ");

    // Non-time filter clauses (scope + optional column filters). These bind
    // once and are referenced verbatim in both halves of the CAGG UNION ALL
    // boundary correction, so each filter parameter is added exactly once.
    let mut filter_clauses: Vec<String> = vec![scope_sql.to_owned()];

    if let Some(ref metric) = query.usage_type {
        *param_idx += 1;
        filter_clauses.push(format!("metric = ${param_idx}"));
        add_arg(args, metric.clone())?;
    }
    if has_id_columns && let Some(resource_id) = query.resource_id {
        *param_idx += 1;
        filter_clauses.push(format!("resource_id = ${param_idx}"));
        add_arg(args, resource_id)?;
    }
    if let Some(ref resource_type) = query.resource_type {
        *param_idx += 1;
        filter_clauses.push(format!("resource_type = ${param_idx}"));
        add_arg(args, resource_type.clone())?;
    }
    if has_id_columns && let Some(subject_id) = query.subject_id {
        *param_idx += 1;
        filter_clauses.push(format!("subject_id = ${param_idx}"));
        add_arg(args, subject_id)?;
    }
    if let Some(ref subject_type) = query.subject_type {
        *param_idx += 1;
        filter_clauses.push(format!("subject_type = ${param_idx}"));
        add_arg(args, subject_type.clone())?;
    }
    if let Some(ref source) = query.source {
        *param_idx += 1;
        filter_clauses.push(format!("module = ${param_idx}"));
        add_arg(args, source.clone())?;
    }

    let filter_sql = filter_clauses.join(" AND ");

    *param_idx += 1;
    let limit_idx = *param_idx;
    let max_rows_limit = i64::try_from(max_agg_rows)
        .map_err(|e| -> UsageCollectorError {
            StoragePluginError::Serialization {
                context: "row limit overflow".to_owned(),
                source: Box::new(e),
            }
            .into()
        })?
        .saturating_add(1);
    add_arg(args, max_rows_limit)?;

    let order_clause = if has_time_bucket {
        " ORDER BY bucket_start ASC"
    } else {
        ""
    };
    let group_clause = if group_by_exprs.is_empty() {
        String::new()
    } else {
        format!(" GROUP BY {}", group_by_exprs.join(", "))
    };

    let (from_clause, where_clause) = if boundary_correction {
        let unified = build_cagg_boundary_corrected_from(
            table,
            time_col,
            &filter_sql,
            time_start_idx,
            time_end_idx,
        );
        (unified, String::from("TRUE"))
    } else {
        let mut where_clauses = Vec::with_capacity(filter_clauses.len() + 2);
        where_clauses.push(scope_sql.to_owned());
        where_clauses.push(format!("{time_col} >= ${time_start_idx}"));
        where_clauses.push(format!("{time_col} {end_op} ${time_end_idx}"));
        for clause in filter_clauses.iter().skip(1) {
            where_clauses.push(clause.clone());
        }
        (table.to_owned(), where_clauses.join(" AND "))
    };

    Ok(format!(
        "SELECT {select_clause} FROM {from_clause} WHERE {where_clause}{group_clause}{order_clause} LIMIT ${limit_idx}"
    ))
}

#[allow(clippy::too_many_lines)]
#[async_trait]
impl QueryPort for PgQueryPort {
    // @cpt-algo:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1
    async fn query_aggregated(
        &self,
        query: AggregationQuery,
    ) -> Result<Vec<AggregationResult>, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-1
        let (scope_sql, scope_params) =
            scope_to_sql(&query.scope).map_err(|e| map_scope_error(&e))?;
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-1

        let group_by = &query.group_by;

        // The gateway validates only that `bucket_size` is present when `group_by`
        // contains `TimeBucket(_)`; it does NOT cross-check that the two carry the
        // same `BucketSize`. Without this guard, a mismatched pair would drive
        // CAGG-vs-raw routing from `bucket_size` while the emitted SQL buckets by
        // `TimeBucket(bs)` — two sources of truth disagreeing silently. Reject
        // the inconsistency at the plugin boundary so the failure mode is a clean
        // `InvalidArgument`, not a wrong result set.
        validate_bucket_size_consistency(query.bucket_size, group_by)?;

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-2
        // The continuous aggregate's buckets are already aligned to 1 hour; rebucketing them
        // at minute granularity yields sparse hour-aligned rows with no data in between.
        // Sub-hour requests must fall back to the raw hypertable.
        let cagg_too_coarse = matches!(query.bucket_size, Some(BucketSize::Minute))
            || group_by
                .iter()
                .any(|d| matches!(d, GroupByDimension::TimeBucket(BucketSize::Minute)));
        // The CAGG view groups out `resource_id` / `subject_id`, so any
        // scope-level filter on those columns must execute against the raw
        // hypertable. Without this check the generated SQL would reference
        // columns that do not exist on the view and fail at runtime.
        let unsafe_time_range =
            !cagg_safe_range(query.time_range.0, query.time_range.1, Utc::now());
        let filter_resource = query.resource_id.is_some();
        let filter_subject = query.subject_id.is_some();
        let group_resource = group_by.contains(&GroupByDimension::Resource);
        let group_subject = group_by.contains(&GroupByDimension::Subject);
        let scope_constrains_ids = scope_constrains_record_ids(&query.scope);
        let use_raw_path = filter_resource
            || filter_subject
            || group_resource
            || group_subject
            || cagg_too_coarse
            || scope_constrains_ids
            || unsafe_time_range;
        // Emit each routing predicate as its own structured field so an
        // operator can distinguish "raw path because filter on resource_id"
        // from "raw path because end is inside the unmaterialized window" —
        // a routing surprise (e.g. a dashboard suddenly slower because every
        // query falls back to raw) is diagnosable from this single event.
        tracing::debug!(
            use_raw_path,
            cagg_too_coarse,
            unsafe_time_range,
            scope_constrains_ids,
            filter_resource,
            filter_subject,
            group_resource,
            group_subject,
            "query_aggregated routing decision"
        );
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-2

        let mut args = PgArguments::default();
        let mut param_idx: usize = 0;

        for sv in scope_params {
            param_idx += 1;
            match sv {
                SqlValue::Uuid(u) => add_arg(&mut args, u)?,
                SqlValue::UuidArray(v) => add_arg(&mut args, v)?,
                SqlValue::Text(s) => add_arg(&mut args, s)?,
                SqlValue::TextArray(v) => add_arg(&mut args, v)?,
            }
        }

        let time_start_idx = param_idx + 1;
        param_idx += 1;
        add_arg(&mut args, query.time_range.0)?;
        let time_end_idx = param_idx + 1;
        param_idx += 1;
        add_arg(&mut args, query.time_range.1)?;

        let has_time_bucket = group_by
            .iter()
            .any(|d| matches!(d, GroupByDimension::TimeBucket(_)));
        let has_usage_type = group_by.contains(&GroupByDimension::UsageType);
        let has_subject = group_by.contains(&GroupByDimension::Subject);
        let has_resource = group_by.contains(&GroupByDimension::Resource);
        let has_source = group_by.contains(&GroupByDimension::Source);

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-3
        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-4
        let variant = if use_raw_path {
            AggSqlVariant {
                table: "usage_records",
                time_col: "timestamp",
                agg_expr: raw_agg_expr(query.function),
                has_id_columns: true,
                end_op: "<=",
                boundary_correction: false,
            }
        } else {
            AggSqlVariant {
                table: CONTINUOUS_AGGREGATE_VIEW,
                time_col: "bucket",
                agg_expr: cagg_agg_expr(query.function),
                has_id_columns: false,
                end_op: "<",
                boundary_correction: true,
            }
        };
        let sql = build_aggregation_sql(
            &variant,
            &query,
            &scope_sql,
            &mut args,
            &mut param_idx,
            TimeRangeArgs {
                start_idx: time_start_idx,
                end_idx: time_end_idx,
            },
            self.max_agg_rows,
        )?;
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-3
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-4

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-5
        let rows = sqlx::query_with(&sql, args)
            .fetch_all(&self.pool)
            .await
            .map_err(classify_query_error)?;
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-5

        // Reject before decode: the SQL `LIMIT` is `max_agg_rows + 1` so an
        // overflow is observable here without spending the work of decoding
        // 10 001 rows into `AggregationResult`s that we are about to discard.
        // The cap is the cost-control invariant; enforcing it after decode
        // would do exactly the work the cap was meant to prevent.
        if rows.len() > self.max_agg_rows {
            return Err(
                UsageRecordError::resource_exhausted("query result too large")
                    .with_quota_violation("rows", "query result row count exceeds limit")
                    .create(),
            );
        }

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-6
        let results: Vec<AggregationResult> = rows
            .iter()
            .map(|row| -> Result<AggregationResult, UsageCollectorError> {
                // Columns that are conditionally selected but NOT NULL in the schema
                // (bucket_start, usage_type/metric, resource_id, resource_type, source/module)
                // are read as `T` so decode errors propagate; only `subject_id` and
                // `subject_type` are NULLable in the underlying table and use `Option<T>`.
                let value = decode::<f64>(row, "agg_value")?;
                let bucket_start = if has_time_bucket {
                    Some(decode(row, "bucket_start")?)
                } else {
                    None
                };
                let usage_type = if has_usage_type {
                    Some(decode(row, "usage_type")?)
                } else {
                    None
                };
                let subject_id = if has_subject && use_raw_path {
                    decode::<Option<Uuid>>(row, "subject_id")?
                } else {
                    None
                };
                let subject_type = if has_subject {
                    decode::<Option<String>>(row, "subject_type")?
                } else {
                    None
                };
                let resource_id = if has_resource && use_raw_path {
                    Some(decode(row, "resource_id")?)
                } else {
                    None
                };
                let resource_type = if has_resource {
                    Some(decode(row, "resource_type")?)
                } else {
                    None
                };
                let source = if has_source {
                    Some(decode(row, "source")?)
                } else {
                    None
                };
                Ok(AggregationResult {
                    function: query.function,
                    value,
                    bucket_start,
                    usage_type,
                    subject_id,
                    subject_type,
                    resource_id,
                    resource_type,
                    source,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-6

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-7
        Ok(results)
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-aggregated:p1:inst-qagg-7
    }

    // @cpt-algo:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1
    async fn query_raw(&self, query: RawQuery) -> Result<Page<UsageRecord>, UsageCollectorError> {
        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-1
        let (scope_sql, scope_params) =
            scope_to_sql(&query.scope).map_err(|e| map_scope_error(&e))?;
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-1

        // Hash is computed lazily — only at the two sites that consume it
        // (cursor-drift detection and next-page cursor emission). The
        // first-page common case (no cursor and no overflow) does not need
        // it; eager computation would do the work the cap was meant to
        // skip. The hash is deterministic for a given filter set, so the
        // (at most) two computations stay self-consistent across the
        // request's lifetime.

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-2
        let cursor_pos: Option<(DateTime<Utc>, Uuid)> = query
            .cursor
            .as_ref()
            .map(|c| {
                if c.d != "fwd" {
                    return Err(UsageRecordError::invalid_argument()
                        .with_constraint("cursor drift: backward pagination not supported")
                        .create());
                }
                if c.o != SortDir::Asc {
                    return Err(UsageRecordError::invalid_argument()
                        .with_constraint("cursor drift: sort direction mismatch")
                        .create());
                }
                if c.s != raw_query_effective_order().to_signed_tokens() {
                    return Err(UsageRecordError::invalid_argument()
                        .with_constraint("cursor drift: sort signature mismatch")
                        .create());
                }
                let expected_filter_hash = raw_query_filter_hash(&RawQueryFilters::from(&query));
                match c.f.as_deref() {
                    Some(h) if h == expected_filter_hash => {}
                    _ => {
                        return Err(UsageRecordError::invalid_argument()
                            .with_constraint("cursor drift: filter set has changed")
                            .create());
                    }
                }
                let ts_str = c.k.first().ok_or_else(|| {
                    UsageRecordError::invalid_argument()
                        .with_constraint("cursor missing timestamp key")
                        .create()
                })?;
                let id_str = c.k.get(1).ok_or_else(|| {
                    UsageRecordError::invalid_argument()
                        .with_constraint("cursor missing id key")
                        .create()
                })?;
                let ts = ts_str.parse::<DateTime<Utc>>().map_err(|e| {
                    UsageRecordError::invalid_argument()
                        .with_constraint(format!("cursor timestamp parse error: {e}"))
                        .create()
                })?;
                let id = id_str.parse::<Uuid>().map_err(|e| {
                    UsageRecordError::invalid_argument()
                        .with_constraint(format!("cursor id parse error: {e}"))
                        .create()
                })?;
                Ok::<_, UsageCollectorError>((ts, id))
            })
            .transpose()?;
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-2

        let mut args = PgArguments::default();
        let mut param_idx: usize = 0;

        for sv in scope_params {
            param_idx += 1;
            match sv {
                SqlValue::Uuid(u) => add_arg(&mut args, u)?,
                SqlValue::UuidArray(v) => add_arg(&mut args, v)?,
                SqlValue::Text(s) => add_arg(&mut args, s)?,
                SqlValue::TextArray(v) => add_arg(&mut args, v)?,
            }
        }

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-3
        let time_start_idx = param_idx + 1;
        param_idx += 1;
        add_arg(&mut args, query.time_range.0)?;
        let time_end_idx = param_idx + 1;
        param_idx += 1;
        add_arg(&mut args, query.time_range.1)?;

        let mut where_clauses: Vec<String> = Vec::new();
        where_clauses.push(scope_sql);
        where_clauses.push(format!("timestamp >= ${time_start_idx}"));
        where_clauses.push(format!("timestamp <= ${time_end_idx}"));

        if let Some(ref metric) = query.usage_type {
            param_idx += 1;
            where_clauses.push(format!("metric = ${param_idx}"));
            add_arg(&mut args, metric.clone())?;
        }
        if let Some(resource_id) = query.resource_id {
            param_idx += 1;
            where_clauses.push(format!("resource_id = ${param_idx}"));
            add_arg(&mut args, resource_id)?;
        }
        if let Some(ref resource_type) = query.resource_type {
            param_idx += 1;
            where_clauses.push(format!("resource_type = ${param_idx}"));
            add_arg(&mut args, resource_type.clone())?;
        }
        if let Some(subject_id) = query.subject_id {
            param_idx += 1;
            where_clauses.push(format!("subject_id = ${param_idx}"));
            add_arg(&mut args, subject_id)?;
        }
        if let Some(ref subject_type) = query.subject_type {
            param_idx += 1;
            where_clauses.push(format!("subject_type = ${param_idx}"));
            add_arg(&mut args, subject_type.clone())?;
        }
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-3

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-4
        if let Some((cursor_ts, cursor_id)) = cursor_pos {
            let ts_idx = param_idx + 1;
            param_idx += 1;
            let id_idx = param_idx + 1;
            param_idx += 1;
            add_arg(&mut args, cursor_ts)?;
            add_arg(&mut args, cursor_id)?;
            where_clauses.push(format!(
                "(timestamp > ${ts_idx} OR (timestamp = ${ts_idx} AND id > ${id_idx}))"
            ));
        }
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-4

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-5
        if query.page_size < 1 {
            return Err(UsageRecordError::invalid_argument()
                .with_constraint("page_size must be >= 1")
                .create());
        }
        if query.page_size > MAX_RAW_PAGE_SIZE {
            return Err(UsageRecordError::invalid_argument()
                .with_constraint(format!(
                    "page_size must be <= {MAX_RAW_PAGE_SIZE}, got: {}",
                    query.page_size
                ))
                .create());
        }
        param_idx += 1;
        let page_size_idx = param_idx;
        let page_size = i64::from(query.page_size);
        add_arg(&mut args, page_size + 1)?;

        let where_clause = where_clauses.join(" AND ");
        let sql = format!(
            "SELECT id, tenant_id, module, kind, metric, value::float8 AS value, timestamp, \
             idempotency_key, resource_id, resource_type, subject_id, subject_type, metadata \
             FROM usage_records \
             WHERE {where_clause} \
             ORDER BY timestamp ASC, id ASC \
             LIMIT ${page_size_idx}"
        );
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-5

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-6
        let rows = sqlx::query_with(&sql, args)
            .fetch_all(&self.pool)
            .await
            .map_err(classify_query_error)?;
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-6

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-7
        let page_size_usize = query.page_size as usize;
        let has_more = rows.len() > page_size_usize;
        let rows_page = if has_more {
            &rows[..page_size_usize]
        } else {
            &rows[..]
        };

        let next_cursor: Option<String> = if has_more {
            if let Some(last_row) = rows_page.last() {
                let last_ts: DateTime<Utc> = decode(last_row, "timestamp")?;
                let last_id: Uuid = decode(last_row, "id")?;
                let cursor = CursorV1 {
                    k: vec![last_ts.to_rfc3339(), last_id.to_string()],
                    o: SortDir::Asc,
                    s: raw_query_effective_order().to_signed_tokens(),
                    f: Some(raw_query_filter_hash(&RawQueryFilters::from(&query))),
                    d: "fwd".to_owned(),
                };
                Some(cursor.encode().map_err(|e| -> UsageCollectorError {
                    StoragePluginError::Serialization {
                        context: "cursor encode error".to_owned(),
                        source: Box::new(e),
                    }
                    .into()
                })?)
            } else {
                None
            }
        } else {
            None
        };

        let records: Vec<UsageRecord> = rows_page
            .iter()
            .map(|row| -> Result<UsageRecord, UsageCollectorError> {
                let kind_str: String = decode(row, "kind")?;
                let kind = match kind_str.as_str() {
                    "counter" => UsageKind::Counter,
                    "gauge" => UsageKind::Gauge,
                    other => {
                        return Err(UsageCollectorError::internal(format!(
                            "unknown kind value in storage: {other}"
                        ))
                        .create());
                    }
                };
                let subject_id: Option<Uuid> = decode(row, "subject_id")?;
                let subject_type: Option<String> = decode(row, "subject_type")?;
                let subject = subject_id.map(|id| Subject {
                    id,
                    r#type: subject_type,
                });
                Ok(UsageRecord {
                    module: decode(row, "module")?,
                    tenant_id: decode(row, "tenant_id")?,
                    metric: decode(row, "metric")?,
                    kind,
                    value: decode::<f64>(row, "value")?,
                    resource_id: decode(row, "resource_id")?,
                    resource_type: decode(row, "resource_type")?,
                    subject,
                    idempotency_key: decode::<Option<String>>(row, "idempotency_key")?
                        .unwrap_or_default(),
                    timestamp: decode(row, "timestamp")?,
                    metadata: decode::<Option<serde_json::Value>>(row, "metadata")?,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-7

        // @cpt-begin:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-8
        Ok(Page::new(
            records,
            PageInfo {
                next_cursor,
                prev_cursor: None,
                limit: u64::from(query.page_size),
            },
        ))
        // @cpt-end:cpt-cf-usage-collector-algo-production-storage-plugin-query-raw:p1:inst-qraw-8
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "pg_query_port_tests.rs"]
mod pg_query_port_tests;
