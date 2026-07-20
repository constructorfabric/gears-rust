//! `PostgreSQL`-backed [`CatalogStore`] over the `usage_type_catalog` table.
//!
//! `create` / `get` / `list` / `delete` are all real `sqlx` over the
//! `usage_type_catalog` table.

// Vendored TimescaleDB raw-SQL backend: `sqlx` is required infra (hypertable
// time-series, `time_bucket` aggregation, keyset pagination — see DESIGN.md). Tenant
// isolation is enforced by hand via parameterized `tenant_id` predicates and an
// allowlisted-identifier query builder (DESIGN.md §Injection-Safe Query Translation),
// not SecureConn/AccessScope.
//
// `de1101`: the `#[cfg(test)] refresh_runs` counter is a production test-hook on the
// store struct (it instruments the live refresh worker), so it stays beside the field
// it counts — it cannot move to the `catalog_store_tests.rs` companion.
#![allow(unknown_lints, de0706_no_direct_sqlx, de1101_tests_in_separate_files)]

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::pool::PoolConnection;
use sqlx::{PgPool, Postgres};
use tokio_util::sync::CancellationToken;
use toolkit::tokio::sync::Notify;
use toolkit_odata::filter::{FilterField, convert_expr_to_filter_node};
use toolkit_odata::{ODataOrderBy, ODataQuery, OrderKey, Page as ODataPage, PageInfo, SortDir};

use usage_collector_sdk::{
    UsageCollectorPluginError, UsageType, UsageTypeFilterField, UsageTypeGtsId,
    is_keyset_safe_type_field,
};

use crate::domain::ports::CatalogStore;
use crate::infra::metrics::{ErrorClass, Metrics, OpDurationGuard, QueryKind, TimedOp};
use crate::infra::storage::entity::UsageTypeRow;
use crate::infra::storage::error::{
    DbErrorClass, acquire_error_clears_readiness, classify_db, db_code_and_constraint, map_sqlx_err,
};
use crate::infra::storage::mapper::{gts_id_str, kind_to_sql, type_row_to_model};
use crate::infra::storage::query::effective_page_size;
use crate::infra::storage::query::keyset::{
    encode_next_cursor, ensure_forward_cursor, keyset_predicate,
};
use crate::infra::storage::query::translate::{
    SqlCtx, bind_one, translate_usage_type_filter, usage_type_column,
};

/// Catalog columns for every `usage_type_catalog` SELECT, in [`UsageTypeRow`]
/// field order. A static const (never caller input), so
/// `sqlx::query_as::<_, UsageTypeRow>` decodes positionally without risk of SQL
/// injection.
const TYPE_COLUMNS: &str = "gts_id, kind, metadata_fields";

/// Default page size when the caller omits `$top` (`query.limit`).
const DEFAULT_PAGE_SIZE: u64 = 100;

/// Upper bound on the FK-violation reference probe (`sample_ref_count`). The
/// count is a coarse diagnostic on the `delete` failure path, so the scan is
/// capped rather than run unbounded over the `usage_records` hypertable; the
/// SPI declares `sample_ref_count` a bounded, plugin-tunable value.
const REF_COUNT_CAP: i64 = 1000;

/// The catalog's fixed sort order (`gts_id` ascending), used to encode the
/// next-page cursor. The catalog list ignores `query.order` by design.
fn gts_id_asc_order() -> ODataOrderBy {
    ODataOrderBy(vec![OrderKey {
        field: "gts_id".to_owned(),
        dir: SortDir::Asc,
    }])
}

/// `sqlx`-backed implementation of [`CatalogStore`] over the
/// `usage_type_catalog` table.
///
/// Like [`PgRecordStore`](crate::infra::storage::record_store::PgRecordStore),
/// every operation acquires through [`Self::timed_acquire`], recording
/// `pool.acquire.duration` and incrementing `tls.handshake.failure.count` on a
/// TLS-handshake failure.
#[derive(Debug, Clone)]
pub struct PgCatalogStore {
    pool: PgPool,
    metrics: Arc<Metrics>,
    /// Gear cancellation token, threaded in so the background gauge refresh
    /// aborts its `count(*)` at shutdown instead of leaking a pooled connection.
    cancel: CancellationToken,
    /// Wakes the single background catalog-size refresh worker. `notify_one`
    /// coalesces a burst of catalog mutations into at most one queued refresh,
    /// so concurrent `create` / `delete` never fan out a `count(*)` task per
    /// mutation against the request pool.
    refresh_signal: Arc<Notify>,
    /// Test-only: counts how many times the worker actually ran the count, so a
    /// unit test can assert burst signals coalesce instead of running per-signal.
    #[cfg(test)]
    refresh_runs: Arc<std::sync::atomic::AtomicUsize>,
}

/// Outcome of one background catalog-size refresh. Surfaced so the worker can
/// stop on cancel and the unit tests can assert the cancellation short-circuit.
#[derive(Debug, PartialEq, Eq)]
enum RefreshOutcome {
    /// The cancellation token fired before the count completed; the gauge keeps
    /// its previous value and no connection is held past cancellation.
    Cancelled,
    /// The `count(*)` ran to completion (success or a logged failure).
    Ran,
}

impl PgCatalogStore {
    /// Build a store over an existing connection pool and spawn its single
    /// background catalog-size refresh worker.
    ///
    /// `cancel` is the gear's cancellation token
    /// ([`GearCtx::cancellation_token`](toolkit::context::GearCtx::cancellation_token));
    /// the refresh worker races its `count(*)` against it so a shutdown drops the
    /// in-flight query, returns its connection promptly, and the worker exits.
    ///
    /// Must be invoked within a Tokio runtime (the worker is spawned eagerly).
    #[must_use]
    pub fn new(pool: PgPool, metrics: Arc<Metrics>, cancel: CancellationToken) -> Self {
        let store = Self {
            pool,
            metrics,
            cancel,
            refresh_signal: Arc::new(Notify::new()),
            #[cfg(test)]
            refresh_runs: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        // One long-lived worker drains coalesced refresh requests; spawned here
        // so exactly one exists per store.
        store.spawn_refresh_worker();
        store
    }

    /// Map a `sqlx` error via [`map_sqlx_err`] and increment the backend-error
    /// counter under the matching [`ErrorClass`] ([`ErrorClass::Transient`] for a
    /// transient mapping, otherwise [`ErrorClass::Internal`]). Returns the mapped
    /// error so it drops into the existing `.map_err(...)` sites unchanged.
    fn record_backend_error(&self, err: &sqlx::Error) -> UsageCollectorPluginError {
        // A TLS handshake failure is the plugin's one metered transport-security
        // signal (DESIGN §Observability); count it before the generic mapping.
        if matches!(err, sqlx::Error::Tls(_)) {
            self.metrics.inc_tls_handshake_failure();
        }
        let mapped = map_sqlx_err(err);
        let class = if matches!(mapped, UsageCollectorPluginError::Transient { .. }) {
            ErrorClass::Transient
        } else {
            ErrorClass::Internal
        };
        self.metrics.inc_backend_error(class);
        mapped
    }

    /// Acquire a pooled connection, recording `pool.acquire.duration`. Errors map
    /// through [`Self::record_backend_error`] (which also catches a TLS-handshake
    /// failure on a fresh physical connection).
    async fn timed_acquire(&self) -> Result<PoolConnection<Postgres>, UsageCollectorPluginError> {
        let t = std::time::Instant::now();
        match self.pool.acquire().await {
            Ok(conn) => {
                self.metrics.record_pool_acquire(t.elapsed().as_secs_f64());
                // A successful acquire re-arms readiness (DESIGN §Observability),
                // but only while not shutting down: once `cancel` fires the
                // shutdown watcher owns the gauge, so a drain-time acquire must
                // not flip it back to 1. This gate narrows — it does not fully
                // close — the check-then-set window against the watcher; that
                // residual race is a sub-tick blip on a best-effort gauge during
                // one-way shutdown, so it is left as-is rather than serialized.
                if !self.cancel.is_cancelled() {
                    self.metrics.set_ready(true);
                }
                Ok(conn)
            }
            Err(e) => {
                // Clear readiness only on a connectivity-class failure so the
                // `uc_timescaledb_ready == 0` alert fires on a live outage but
                // not on a healthy-but-saturated pool (`PoolTimedOut` while the
                // pool still holds connections), which would otherwise flap the
                // gauge under load.
                if acquire_error_clears_readiness(&e, self.pool.size()) {
                    self.metrics.set_ready(false);
                }
                Err(self.record_backend_error(&e))
            }
        }
    }

    /// Request a best-effort catalog-size gauge refresh **off** the request path.
    ///
    /// The gauge is observability, not a contract, so `create` / `delete` return
    /// without awaiting the `count(*)`. Rather than spawning a task per call —
    /// which under catalog churn fans out many concurrent `count(*)` queries all
    /// competing for the request pool — this only signals the single background
    /// worker. `notify_one` coalesces a burst into at most one queued refresh (one
    /// in-flight plus one trailing run), so the gauge converges to the post-burst
    /// size without flooding the pool.
    fn request_catalog_size_refresh(&self) {
        self.refresh_signal.notify_one();
    }

    /// Spawn the single long-lived worker that drains refresh requests.
    ///
    /// The worker parks on [`Notify::notified`] until a mutation signals it, runs
    /// one cancellable refresh, then loops. A `notify_one` permit stored while it
    /// was busy is consumed on the next iteration — the trailing run that reflects
    /// the latest size. It exits when the cancellation token fires (parked or
    /// mid-`count(*)`), dropping its store clone and pool handle.
    fn spawn_refresh_worker(&self) {
        let store = self.clone();
        toolkit::tokio::spawn(async move {
            loop {
                toolkit::tokio::select! {
                    biased;
                    () = store.cancel.cancelled() => return,
                    () = store.refresh_signal.notified() => {}
                }
                // A shutdown racing the signal still short-circuits the count.
                if store.refresh_catalog_size_cancellable().await == RefreshOutcome::Cancelled {
                    return;
                }
            }
        });
    }

    /// Race [`Self::refresh_catalog_size`] against the cancellation token.
    ///
    /// On cancel the `count(*)` future is dropped — returning its connection to
    /// the pool — and the gauge keeps its previous value. Returns which arm won so
    /// the worker stops on cancel and the unit tests can assert the short-circuit.
    async fn refresh_catalog_size_cancellable(&self) -> RefreshOutcome {
        toolkit::tokio::select! {
            biased;
            () = self.cancel.cancelled() => RefreshOutcome::Cancelled,
            () = self.refresh_catalog_size() => RefreshOutcome::Ran,
        }
    }

    /// Refresh the catalog-size gauge from a live `count(*)`.
    ///
    /// Best-effort: a failed count leaves the previous gauge value in place,
    /// increments the backend-error counter (so a persistently wedged refresh is
    /// visible to alerting rather than silent), and is logged at `warn` rather
    /// than surfaced (the gauge is observability, not a contract). The count is
    /// read as a `bigint`/`i64` and converted to `u64` via `try_from` (no `as`
    /// cast), flooring a negative (impossible) value at `0`.
    async fn refresh_catalog_size(&self) {
        #[cfg(test)]
        self.refresh_runs
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match sqlx::query_scalar::<_, i64>("SELECT count(*)::bigint FROM usage_type_catalog")
            .fetch_one(&self.pool)
            .await
        {
            Ok(n) => self.metrics.set_catalog_size(u64::try_from(n).unwrap_or(0)),
            Err(e) => {
                // Count and classify the failure like every other query path
                // (mapped error discarded — this refresh is off the request path,
                // so there is no caller to return it to).
                let _ = self.record_backend_error(&e);
                tracing::warn!(error = %e, "failed to refresh usage_type_catalog size gauge");
            }
        }
    }
}

#[async_trait]
impl CatalogStore for PgCatalogStore {
    // @cpt-flow:cpt-cf-uc-plugin-seq-create-type:p2
    async fn create(&self, usage_type: UsageType) -> Result<UsageType, UsageCollectorPluginError> {
        let metadata_fields: Vec<String> = usage_type
            .metadata_fields
            .iter()
            .map(|key| key.as_str().to_owned())
            .collect();

        let mut conn = self.timed_acquire().await?;
        let result = sqlx::query(
            "INSERT INTO usage_type_catalog (gts_id, kind, metadata_fields) VALUES ($1, $2, $3)",
        )
        .bind(gts_id_str(&usage_type.gts_id))
        .bind(kind_to_sql(usage_type.kind))
        .bind(&metadata_fields)
        .execute(&mut *conn)
        .await;

        match result {
            Ok(_) => {
                // Refresh the catalog-size gauge off the request path.
                self.request_catalog_size_refresh();
                Ok(usage_type)
            }
            Err(err) => {
                if let Some((code, constraint)) = db_code_and_constraint(&err)
                    && classify_db(&code, constraint.as_deref())
                        == DbErrorClass::CatalogUniqueViolation
                {
                    return Err(UsageCollectorPluginError::UsageTypeAlreadyExists {
                        gts_id: usage_type.gts_id,
                    });
                }
                Err(self.record_backend_error(&err))
            }
        }
    }

    async fn get(&self, gts_id: UsageTypeGtsId) -> Result<UsageType, UsageCollectorPluginError> {
        let mut conn = self.timed_acquire().await?;
        let row = sqlx::query_as::<_, UsageTypeRow>(
            "SELECT gts_id, kind, metadata_fields FROM usage_type_catalog WHERE gts_id = $1",
        )
        .bind(gts_id_str(&gts_id))
        .fetch_optional(&mut *conn)
        .await
        .map_err(|err| self.record_backend_error(&err))?;

        match row {
            Some(row) => type_row_to_model(row),
            None => Err(UsageCollectorPluginError::UsageTypeNotFound { gts_id }),
        }
    }

    /// Keyset-paginated `usage_type_catalog` list, fixed-ordered by `gts_id`
    /// ascending (the catalog's stable design order; `query.order` is ignored).
    ///
    /// Builds `SELECT {TYPE_COLUMNS} FROM usage_type_catalog [WHERE <filter>
    /// [AND <keyset>]] ORDER BY gts_id ASC LIMIT <n+1>`. The catalog is not
    /// tenant/gts scoped, so the bind context starts at `$1` (no leading scope
    /// bind) and there is no metadata side channel. The extra look-ahead row
    /// detects a following page and is truncated before mapping. Identifiers
    /// come from the [`usage_type_column`] allowlist and the static
    /// [`TYPE_COLUMNS`]; every value is bound.
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorPluginError::Internal`] when the filter AST
    /// references an unknown field, the cursor's filter hash disagrees with
    /// `query.filter_hash`, a stored row cannot be mapped, or the DB query
    /// fails.
    async fn list(
        &self,
        query: &ODataQuery,
    ) -> Result<ODataPage<UsageType>, UsageCollectorPluginError> {
        // The catalog list is a raw read; count the request and let the
        // drop-timer record the histogram on every return, not just on success.
        let _timer =
            OpDurationGuard::start(Arc::clone(&self.metrics), TimedOp::Query(QueryKind::Raw));
        self.metrics.inc_query_request(QueryKind::Raw);
        // Defense-in-depth: clamp the caller's `$top` to `MAX_PAGE_SIZE` (see
        // [`effective_page_size`]). The catalog is small/global, so this is a
        // backstop rather than a live scan concern, but it keeps both list
        // paths consistent.
        let limit = effective_page_size(query.limit, DEFAULT_PAGE_SIZE);

        // No leading scope bind: catalog list is not tenant/gts scoped.
        let mut ctx = SqlCtx::new(1);
        let mut clauses: Vec<String> = Vec::new();

        // Optional `$filter`.
        if let Some(expr) = query.filter() {
            let node = convert_expr_to_filter_node::<UsageTypeFilterField>(expr)
                .map_err(|e| UsageCollectorPluginError::internal(format!("invalid filter: {e}")))?;
            let fragment = translate_usage_type_filter(&node, &mut ctx)
                .map_err(UsageCollectorPluginError::internal)?;
            clauses.push(fragment);
        }

        // Keyset continuation over the single `gts_id` ascending key.
        if let Some(cursor) = query.cursor.as_ref() {
            // Forward-only: the keyset operator is derived from the sort
            // direction, not from `cursor.d`, so a backward cursor would
            // silently page forward. Reject it fail-closed.
            ensure_forward_cursor(cursor).map_err(UsageCollectorPluginError::internal)?;
            if cursor.f.as_deref() != query.filter_hash.as_deref() {
                return Err(UsageCollectorPluginError::internal(
                    "cursor filter hash mismatch",
                ));
            }
            let predicate = keyset_predicate(
                &[("gts_id", true)],
                &cursor.k,
                usage_type_column,
                |name| UsageTypeFilterField::from_name(name).map(|f| f.kind()),
                is_keyset_safe_type_field,
                &mut ctx,
            )
            .map_err(UsageCollectorPluginError::internal)?;
            clauses.push(predicate);
        }

        let where_sql = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };

        let sql = format!(
            "SELECT {TYPE_COLUMNS} FROM usage_type_catalog{where_sql} \
             ORDER BY gts_id ASC LIMIT {}",
            limit.saturating_add(1),
        );

        let mut q = sqlx::query_as::<_, UsageTypeRow>(&sql);
        for b in &ctx.binds {
            q = bind_one(q, b);
        }
        let mut conn = self.timed_acquire().await?;
        let mut rows = q
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| self.record_backend_error(&e))?;

        // Look-ahead row present -> a next page exists; drop it before mapping.
        let has_next = rows.len() > usize::try_from(limit).unwrap_or(usize::MAX);
        if has_next {
            rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        }

        let next_cursor = if has_next {
            let last = rows.last().ok_or_else(|| {
                UsageCollectorPluginError::internal("non-empty page lost its tail")
            })?;
            let order = gts_id_asc_order();
            let token = encode_next_cursor(
                &order,
                std::slice::from_ref(&last.gts_id),
                query.filter_hash.as_deref(),
            )
            .map_err(UsageCollectorPluginError::internal)?;
            Some(token)
        } else {
            None
        };

        let items = rows
            .into_iter()
            .map(type_row_to_model)
            .collect::<Result<Vec<_>, _>>()?;

        // `_timer` records `query.duration` on drop (success and error alike).
        Ok(ODataPage::new(
            items,
            PageInfo {
                next_cursor,
                prev_cursor: None,
                limit,
            },
        ))
    }

    // @cpt-flow:cpt-cf-uc-plugin-seq-delete-type-fk:p2
    async fn delete(&self, gts_id: UsageTypeGtsId) -> Result<(), UsageCollectorPluginError> {
        let mut conn = self.timed_acquire().await?;
        let result = sqlx::query("DELETE FROM usage_type_catalog WHERE gts_id = $1")
            .bind(gts_id_str(&gts_id))
            .execute(&mut *conn)
            .await;

        match result {
            Ok(done) => {
                if done.rows_affected() == 0 {
                    return Err(UsageCollectorPluginError::UsageTypeNotFound { gts_id });
                }
                // Refresh the catalog-size gauge off the request path.
                self.request_catalog_size_refresh();
                Ok(())
            }
            Err(err) => {
                if let Some((code, constraint)) = db_code_and_constraint(&err)
                    && classify_db(&code, constraint.as_deref())
                        == DbErrorClass::ForeignKeyViolation
                {
                    // The DELETE failed on the FK guard; the connection is still
                    // usable, so reuse it for the reference count. The probe is
                    // capped at `REF_COUNT_CAP` so it never scans the whole
                    // `usage_records` hypertable just to fill a coarse sample.
                    let count: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM \
                         (SELECT 1 FROM usage_records WHERE gts_id = $1 LIMIT $2) sub",
                    )
                    .bind(gts_id_str(&gts_id))
                    .bind(REF_COUNT_CAP)
                    .fetch_one(&mut *conn)
                    .await
                    .map_err(|count_err| self.record_backend_error(&count_err))?;
                    let sample_ref_count = u64::try_from(count).unwrap_or(1).max(1);
                    self.metrics.inc_usage_type_referenced();
                    return Err(UsageCollectorPluginError::UsageTypeReferenced {
                        gts_id,
                        sample_ref_count,
                    });
                }
                Err(self.record_backend_error(&err))
            }
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "catalog_store_tests.rs"]
mod catalog_store_tests;
