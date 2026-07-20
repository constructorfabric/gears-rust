//! Postgres-backed [`RecordStore`] over the `usage_records` hypertable.
//!
//! All operations — `create` / `create_batch` / `get` / `list` / `aggregate` /
//! `deactivate` — are real `sqlx`.

// Vendored TimescaleDB raw-SQL backend: `sqlx` is required infra (hypertable
// time-series, `time_bucket` aggregation, keyset pagination — see DESIGN.md). Tenant
// isolation is enforced by hand via parameterized `tenant_id` predicates and an
// allowlisted-identifier query builder (DESIGN.md §Injection-Safe Query Translation),
// not SecureConn/AccessScope.
#![allow(unknown_lints, de0706_no_direct_sqlx)]

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bigdecimal::BigDecimal;
use rand::RngExt as _;
use rust_decimal::Decimal;
use sqlx::pool::PoolConnection;
use sqlx::{Connection, PgPool, Postgres, Row};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio_util::sync::CancellationToken;
use toolkit_odata::filter::{FilterField, convert_expr_to_filter_node};
use toolkit_odata::{ODataQuery, Page as ODataPage, PageInfo, SortDir};
use uuid::Uuid;

use usage_collector_sdk::{
    AggregationBucket, AggregationDimension, AggregationResult, AggregationSpec, MetadataFilter,
    UsageCollectorPluginError, UsageRecord, UsageRecordFilterField, UsageTypeGtsId,
    is_keyset_safe_record_field,
};

use crate::domain::ports::RecordStore;
use crate::infra::metrics::{ErrorClass, InsertMode, Metrics, OpDurationGuard, QueryKind, TimedOp};
use crate::infra::storage::entity::UsageRecordRow;
use crate::infra::storage::error::{
    DbErrorClass, acquire_error_clears_readiness, classify_db, db_code_and_constraint, map_sqlx_err,
};
use crate::infra::storage::mapper::{
    gts_id_str, metadata_jsonb_to_map, metadata_map_to_jsonb, record_row_to_model,
};
use crate::infra::storage::query::aggregate::{
    agg_select_expr, aggregate_limit_clause, corrects_id_partition_clause, dimension_select_expr,
};
use crate::infra::storage::query::effective_page_size;
use crate::infra::storage::query::keyset::{
    encode_next_cursor, ensure_forward_cursor, keyset_predicate, render_order_by,
};
use crate::infra::storage::query::translate::{
    SqlBind, SqlCtx, bind_one, bind_one_query, record_column, translate_record_filter,
};

/// Default page size when the caller omits `$top` (`query.limit`).
const DEFAULT_PAGE_SIZE: u64 = 100;

/// Column list for every `usage_records` SELECT / RETURNING, in
/// [`UsageRecordRow`] field order. A static const (never caller input), so
/// `sqlx::query_as::<_, UsageRecordRow>` decodes positionally without risk of
/// SQL injection.
const RECORD_COLUMNS: &str = "id, tenant_id, gts_id, value, created_at, resource_id, \
     resource_type, subject_id, subject_type, idempotency_key, corrects_id, status, metadata, \
     ingested_at";

/// `sqlx`-backed implementation of [`RecordStore`] over the `usage_records`
/// hypertable.
///
/// Every operation acquires its connection through [`Self::timed_acquire`], so
/// `pool.acquire.duration` is recorded per acquire and `tls.handshake.failure.count`
/// is incremented when a fresh physical connection fails its TLS handshake (via
/// [`Self::record_backend_error`]).
#[derive(Debug, Clone)]
pub struct PgRecordStore {
    pool: PgPool,
    metrics: Arc<Metrics>,
    cancel: CancellationToken,
}

impl PgRecordStore {
    /// Build a store over an existing connection pool. `cancel` is the gear's
    /// cancellation token; the request path stops re-arming the `ready` gauge
    /// once it fires so a drain-time acquire cannot flip readiness back on after
    /// the shutdown watcher has cleared it.
    #[must_use]
    pub fn new(pool: PgPool, metrics: Arc<Metrics>, cancel: CancellationToken) -> Self {
        Self {
            pool,
            metrics,
            cancel,
        }
    }

    /// Map a `sqlx` error via [`map_sqlx_err`] and, as a side effect, increment
    /// the backend-error counter under the matching [`ErrorClass`]
    /// ([`ErrorClass::Transient`] for a [`UsageCollectorPluginError::Transient`]
    /// mapping, otherwise [`ErrorClass::Internal`]). Returns the mapped error so
    /// it slots into the existing `.map_err(...)` call sites unchanged.
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

    /// Single-row insert error mapping. A foreign-key violation on
    /// `usage_records.gts_id` means the referenced usage type is absent from the
    /// catalog — the narrow TOCTOU race where it is deleted between the core's
    /// pre-insert catalog existence check and this insert. Surface it as the
    /// typed [`UsageCollectorPluginError::UsageTypeNotFound`] (the core lifts it
    /// to a 404) instead of a generic Internal (500); every other error falls
    /// through to [`Self::record_backend_error`] (which also meters it). Mirrors
    /// the catalog-store FK → `UsageTypeReferenced` mapping. The batch path is
    /// intentionally excluded: a multi-`gts_id` UNNEST insert cannot attribute a
    /// single FK violation to one `gts_id`, so a typed mapping there would lie.
    fn map_insert_error(
        &self,
        err: &sqlx::Error,
        gts_id: &UsageTypeGtsId,
    ) -> UsageCollectorPluginError {
        if let Some((code, constraint)) = db_code_and_constraint(err)
            && classify_db(&code, constraint.as_deref()) == DbErrorClass::ForeignKeyViolation
        {
            return UsageCollectorPluginError::UsageTypeNotFound {
                gts_id: gts_id.clone(),
            };
        }
        self.record_backend_error(err)
    }

    /// Acquire a pooled connection, recording `pool.acquire.duration`. Errors map
    /// through [`Self::record_backend_error`] (which also catches a TLS-handshake
    /// failure on a fresh physical connection). Every operation acquires through
    /// this path so the acquire-latency histogram is representative.
    async fn timed_acquire(&self) -> Result<PoolConnection<Postgres>, UsageCollectorPluginError> {
        let t = Instant::now();
        match self.pool.acquire().await {
            Ok(conn) => {
                self.metrics.record_pool_acquire(t.elapsed().as_secs_f64());
                // A successful acquire re-arms readiness (DESIGN §Observability:
                // `ready` recovers once the pool serves a connection again), but
                // only while not shutting down: once `cancel` fires the shutdown
                // watcher owns the gauge, so a drain-time acquire must not flip it
                // back to 1. This gate narrows — it does not fully close — the
                // check-then-set window against the watcher; that residual race
                // is a sub-tick blip on a best-effort gauge during one-way
                // shutdown, so it is left as-is rather than serialized.
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

    /// Core single-row insert path: dedup on the `usage_records`
    /// `(tenant_id, gts_id, idempotency_key, created_at)` UNIQUE
    /// (`usage_records_dedup_uniq`) via `INSERT … ON CONFLICT … DO NOTHING`, then
    /// lost-the-race absorb-vs-conflict resolution.
    ///
    /// `ON CONFLICT DO NOTHING` is the serialization authority: a concurrent
    /// same-key insert blocks on the in-progress speculative tuple until the
    /// winner commits, then its `DO NOTHING` returns no row and it resolves
    /// absorb-vs-conflict against the now-visible committed row. The operation is
    /// one `INSERT` plus at most one `SELECT` (both read-committed), so no
    /// explicit transaction is needed.
    ///
    /// This carries the per-row counters (dedup absorbed / idempotency conflict
    /// / compensation / backend error) so they are recorded exactly once per
    /// row whether the caller is [`RecordStore::create`] (single) or
    /// [`RecordStore::create_batch`] (per-row loop). The `insert.duration`
    /// histogram is deliberately NOT recorded here — the public methods time
    /// the whole call and tag it with the correct `mode`.
    async fn create_inner(
        &self,
        record: UsageRecord,
    ) -> Result<UsageRecord, UsageCollectorPluginError> {
        let mut conn = self.timed_acquire().await?;

        // 1. Insert, deduplicated on the 4-tuple UNIQUE. `RETURNING` yields the
        //    row only when we won the slot — `DO NOTHING` suppresses it on a
        //    conflict — so `Some` = fresh insert, `None` = a row with this
        //    4-tuple already exists.
        let insert_sql = format!(
            "INSERT INTO usage_records \
             (id, tenant_id, gts_id, value, created_at, resource_id, resource_type, \
              subject_id, subject_type, idempotency_key, corrects_id, metadata) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) \
             ON CONFLICT (tenant_id, gts_id, idempotency_key, created_at) DO NOTHING \
             RETURNING {RECORD_COLUMNS}"
        );
        let subject_id = record
            .subject_ref
            .as_ref()
            .map(usage_collector_sdk::SubjectRef::subject_id);
        let subject_type = record.subject_ref.as_ref().and_then(|s| s.subject_type());
        let metadata = metadata_map_to_jsonb(&record.metadata);
        let is_compensation = record.corrects_id.is_some();

        let inserted = sqlx::query_as::<_, UsageRecordRow>(&insert_sql)
            .bind(record.id)
            .bind(record.tenant_id)
            .bind(gts_id_str(&record.gts_id))
            .bind(record.value)
            .bind(record.created_at)
            .bind(record.resource_ref.resource_id())
            .bind(record.resource_ref.resource_type())
            .bind(subject_id)
            .bind(subject_type)
            .bind(record.idempotency_key.as_str())
            .bind(record.corrects_id)
            .bind(metadata)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| self.map_insert_error(&e, &record.gts_id))?;

        if let Some(row) = inserted {
            // 2a. Won the slot — fresh insert.
            if is_compensation {
                self.metrics.inc_compensation();
            }
            return record_row_to_model(row);
        }

        // 2b. Lost the slot — a row with this 4-tuple already exists. Read it and
        //     resolve absorb-vs-conflict. The read mutates nothing.
        let select_sql = format!(
            "SELECT {RECORD_COLUMNS} FROM usage_records \
             WHERE tenant_id = $1 AND gts_id = $2 AND idempotency_key = $3 AND created_at = $4"
        );
        let stored = sqlx::query_as::<_, UsageRecordRow>(&select_sql)
            .bind(record.tenant_id)
            .bind(gts_id_str(&record.gts_id))
            .bind(record.idempotency_key.as_str())
            .bind(record.created_at)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| self.record_backend_error(&e))?;

        if let Some(row) = stored {
            self.resolve_dedup_hit(row, &record)
        } else {
            // Stale: the conflicting row's chunk was dropped by retention between
            // the conflicting insert and this read — a near-impossible race
            // against the retention boundary (the unique entry is dropped with
            // the chunk, so a retry now wins the freed slot as a fresh insert).
            // Return retryable Transient.
            self.metrics.inc_dedup_stale();
            Err(dedup_transient(
                &record,
                "conflicting record aged out during dedup resolution; retry",
            ))
        }
    }

    /// Resolve a dedup-key hit into absorb (stored row) vs `IdempotencyConflict`
    /// via [`canonical_equal`]. Called from the conflict branch of
    /// `create_inner` when an existing dedup slot's stored record is found.
    /// Increments the matching per-row counter: `dedup.absorbed` on an
    /// exact-equality absorb, `idempotency.conflict` on a canonical-field
    /// mismatch. A stored-metadata decode failure propagates as `Internal`
    /// rather than masquerading as a conflict.
    fn resolve_dedup_hit(
        &self,
        row: UsageRecordRow,
        record: &UsageRecord,
    ) -> Result<UsageRecord, UsageCollectorPluginError> {
        if canonical_equal(&row, record)? {
            // Exact-equality retry — silently absorb, returning the stored row.
            self.metrics.inc_dedup_absorbed();
            record_row_to_model(row)
        } else {
            self.metrics.inc_idempotency_conflict();
            Err(UsageCollectorPluginError::IdempotencyConflict {
                idempotency_key: record.idempotency_key.as_str().to_owned(),
                existing_id: row.id,
            })
        }
    }

    /// Insert all distinct-key representatives in one multi-row
    /// `INSERT … ON CONFLICT (4-tuple) DO NOTHING RETURNING`. The returned rows
    /// are exactly the slots we won — `DO NOTHING` suppresses any row whose
    /// `(tenant_id, gts_id, idempotency_key, created_at)` already exists — so the
    /// result maps each won [`DedupKey`] to its stored row. `reps` must be sorted
    /// by [`DedupKey`] so concurrent batches insert in one global order
    /// (deadlock-free). `metadata` is bound as `text[]` of JSON strings and cast
    /// `::jsonb` per-row to sidestep `jsonb[]` array encoding.
    async fn insert_records_on_conflict(
        &self,
        conn: &mut sqlx::PgConnection,
        reps: &[&UsageRecord],
    ) -> Result<HashMap<DedupKey, UsageRecordRow>, UsageCollectorPluginError> {
        if reps.is_empty() {
            return Ok(HashMap::new());
        }

        let ids: Vec<Uuid> = reps.iter().map(|r| r.id).collect();
        let tenants: Vec<Uuid> = reps.iter().map(|r| r.tenant_id).collect();
        let gtss: Vec<String> = reps
            .iter()
            .map(|r| gts_id_str(&r.gts_id).to_owned())
            .collect();
        let values: Vec<Decimal> = reps.iter().map(|r| r.value).collect();
        let cats: Vec<OffsetDateTime> = reps.iter().map(|r| r.created_at).collect();
        let resource_ids: Vec<String> = reps
            .iter()
            .map(|r| r.resource_ref.resource_id().to_owned())
            .collect();
        let resource_types: Vec<String> = reps
            .iter()
            .map(|r| r.resource_ref.resource_type().to_owned())
            .collect();
        let subject_ids: Vec<Option<String>> = reps
            .iter()
            .map(|r| {
                r.subject_ref
                    .as_ref()
                    .map(|s| usage_collector_sdk::SubjectRef::subject_id(s).to_owned())
            })
            .collect();
        let subject_types: Vec<Option<String>> = reps
            .iter()
            .map(|r| {
                r.subject_ref
                    .as_ref()
                    .and_then(|s| s.subject_type())
                    .map(str::to_owned)
            })
            .collect();
        let idem_keys: Vec<String> = reps
            .iter()
            .map(|r| r.idempotency_key.as_str().to_owned())
            .collect();
        let corrects: Vec<Option<Uuid>> = reps.iter().map(|r| r.corrects_id).collect();
        let metadata: Vec<String> = reps
            .iter()
            .map(|r| metadata_map_to_jsonb(&r.metadata).to_string())
            .collect();

        let sql = format!(
            "INSERT INTO usage_records \
             (id, tenant_id, gts_id, value, created_at, resource_id, resource_type, \
              subject_id, subject_type, idempotency_key, corrects_id, metadata) \
             SELECT id, tenant_id, gts_id, value, created_at, resource_id, resource_type, \
              subject_id, subject_type, idempotency_key, corrects_id, metadata::jsonb \
             FROM UNNEST($1::uuid[], $2::uuid[], $3::text[], $4::numeric[], $5::timestamptz[], \
              $6::text[], $7::text[], $8::text[], $9::text[], $10::text[], $11::uuid[], $12::text[]) \
              AS t(id, tenant_id, gts_id, value, created_at, resource_id, resource_type, \
                   subject_id, subject_type, idempotency_key, corrects_id, metadata) \
             ON CONFLICT (tenant_id, gts_id, idempotency_key, created_at) DO NOTHING \
             RETURNING {RECORD_COLUMNS}"
        );

        let rows = sqlx::query_as::<_, UsageRecordRow>(&sql)
            .bind(&ids)
            .bind(&tenants)
            .bind(&gtss)
            .bind(&values)
            .bind(&cats)
            .bind(&resource_ids)
            .bind(&resource_types)
            .bind(&subject_ids)
            .bind(&subject_types)
            .bind(&idem_keys)
            .bind(&corrects)
            .bind(&metadata)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| self.record_backend_error(&e))?;

        Ok(rows
            .into_iter()
            .map(|row| (row_dedup_key(&row), row))
            .collect())
    }

    /// For the not-won keys, read the existing `usage_records` row by its 4-tuple
    /// `(tenant_id, gts_id, idempotency_key, created_at)` — the batch analogue of
    /// the single path's conflict branch. Maps each key to `Stored` (row found →
    /// resolve absorb/conflict) or `Stale` (the conflicting row's chunk was
    /// dropped by retention between the conflicting insert and this read).
    async fn read_conflict_records(
        &self,
        conn: &mut sqlx::PgConnection,
        not_won: &[&UsageRecord],
    ) -> Result<HashMap<DedupKey, ConflictRead>, UsageCollectorPluginError> {
        let mut out: HashMap<DedupKey, ConflictRead> = HashMap::new();
        if not_won.is_empty() {
            return Ok(out);
        }

        let tenants: Vec<Uuid> = not_won.iter().map(|r| r.tenant_id).collect();
        let gtss: Vec<String> = not_won
            .iter()
            .map(|r| gts_id_str(&r.gts_id).to_owned())
            .collect();
        let keys: Vec<String> = not_won
            .iter()
            .map(|r| r.idempotency_key.as_str().to_owned())
            .collect();
        let cats: Vec<OffsetDateTime> = not_won.iter().map(|r| r.created_at).collect();

        let select_sql = format!(
            "SELECT {RECORD_COLUMNS} FROM usage_records \
             WHERE (tenant_id, gts_id, idempotency_key, created_at) IN \
               (SELECT t1, t2, t3, t4 \
                FROM UNNEST($1::uuid[], $2::text[], $3::text[], $4::timestamptz[]) \
                  AS t(t1, t2, t3, t4))"
        );
        let rows = sqlx::query_as::<_, UsageRecordRow>(&select_sql)
            .bind(&tenants)
            .bind(&gtss)
            .bind(&keys)
            .bind(&cats)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| self.record_backend_error(&e))?;

        let mut found: HashMap<DedupKey, UsageRecordRow> = HashMap::new();
        for row in rows {
            found.insert(row_dedup_key(&row), row);
        }

        // Every not-won key resolves to Stored (its conflicting row was read) or
        // Stale (the row's chunk was dropped by retention between the conflicting
        // insert and this read — the near-impossible retention-boundary race).
        // `reps` are distinct keys, so each `remove` is unambiguous.
        for r in not_won {
            let key = dedup_key(r);
            match found.remove(&key) {
                Some(row) => {
                    out.insert(key, ConflictRead::Stored(Box::new(row)));
                }
                None => {
                    out.insert(key, ConflictRead::Stale);
                }
            }
        }

        Ok(out)
    }

    /// Resolve every input row in original order against its authoritative
    /// record, recording the per-row counters exactly as the single path does.
    fn resolve_batch(
        &self,
        records: &[UsageRecord],
        plan: &BatchPlan<'_>,
        won: &HashSet<DedupKey>,
        inserted: &HashMap<DedupKey, UsageRecordRow>,
        conflict: &HashMap<DedupKey, ConflictRead>,
    ) -> Vec<Result<UsageRecord, UsageCollectorPluginError>> {
        let mut results = Vec::with_capacity(records.len());
        for (i, record) in records.iter().enumerate() {
            let key = dedup_key(record);
            let is_winner = won.contains(&key) && plan.first_index.get(&key) == Some(&i);
            let outcome = if is_winner {
                match inserted.get(&key) {
                    Some(row) => {
                        if record.corrects_id.is_some() {
                            self.metrics.inc_compensation();
                        }
                        record_row_to_model(row.clone())
                    }
                    None => Err(dedup_invariant_break(
                        record,
                        "won dedup slot but no inserted record was returned \
                         (concurrent-insert invariant break)",
                    )),
                }
            } else if won.contains(&key) {
                match inserted.get(&key) {
                    Some(row) => self.resolve_dedup_hit(row.clone(), record),
                    None => Err(dedup_invariant_break(
                        record,
                        "intra-batch duplicate of a won key with no inserted record",
                    )),
                }
            } else {
                match conflict.get(&key) {
                    Some(ConflictRead::Stored(row)) => {
                        // Clone the inner row directly; `*row.clone()` would
                        // round-trip through a throwaway `Box` allocation. The
                        // clone itself is required — a not-won key may be
                        // resolved by several input rows against the borrowed map.
                        self.resolve_dedup_hit((**row).clone(), record)
                    }
                    Some(ConflictRead::Stale) => {
                        self.metrics.inc_dedup_stale();
                        Err(dedup_transient(
                            record,
                            "conflicting record aged out during dedup resolution; retry",
                        ))
                    }
                    // Defensive: read_conflict_records populates every not-won key
                    // as Stored or Stale, so a missing entry is unreachable —
                    // surface it as retryable rather than as a silent success.
                    None => Err(dedup_transient(
                        record,
                        "conflicting record not found during dedup resolution; retry",
                    )),
                }
            };
            results.push(outcome);
        }
        results
    }

    /// Orchestrate one batch on a single connection: insert (dedup on the
    /// 4-tuple UNIQUE) → read conflicts for the not-won keys → resolve per row in
    /// input order. The multi-row `INSERT … ON CONFLICT DO NOTHING` is itself
    /// atomic, so no explicit transaction is required; `won` is the set of keys
    /// the insert actually claimed (its `RETURNING` rows).
    async fn create_batch_inner(
        &self,
        records: &[UsageRecord],
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorPluginError>>, UsageCollectorPluginError>
    {
        let plan = plan_batch(records);

        let mut conn = self.timed_acquire().await?;

        let inserted = self
            .insert_records_on_conflict(&mut conn, &plan.reps)
            .await?;
        let won: HashSet<DedupKey> = inserted.keys().cloned().collect();
        let not_won: Vec<&UsageRecord> = plan
            .reps
            .iter()
            .copied()
            .filter(|r| !won.contains(&dedup_key(r)))
            .collect();
        let conflict = self.read_conflict_records(&mut conn, &not_won).await?;

        Ok(self.resolve_batch(records, &plan, &won, &inserted, &conflict))
    }
}

/// Append the metadata side-channel filters as parameterized `WHERE` clauses.
///
/// Shared by [`PgRecordStore::list`] and [`PgRecordStore::aggregate`] so both
/// expand the side channel identically: AND across filters, OR within one
/// filter's values (`metadata ->> $key IN ($v1, $v2, …)`). The key and every
/// value are bound via `ctx` (`$N`); only the `metadata ->> $N` shape is
/// interpolated, so this is injection-safe. An empty value set matches nothing
/// (the gateway rejects it, but be defensive): a `FALSE` clause is emitted so
/// the result is empty rather than unfiltered.
fn push_metadata_filter_clauses(
    metadata_filter: &[MetadataFilter],
    ctx: &mut SqlCtx,
    clauses: &mut Vec<String>,
) {
    for mf in metadata_filter {
        if mf.values().is_empty() {
            clauses.push("FALSE".to_owned());
            continue;
        }
        let key_n = ctx.push(SqlBind::Str(mf.key().as_str().to_owned()));
        let placeholders = mf
            .values()
            .iter()
            .map(|v| format!("${}", ctx.push(SqlBind::Str(v.clone()))))
            .collect::<Vec<_>>();
        clauses.push(format!(
            "metadata ->> ${key_n} IN ({})",
            placeholders.join(", ")
        ));
    }
}

/// Extract a single order-field value from a row as its cursor-key string.
///
/// Inverse of [`cursor_key_to_bind`](crate::infra::storage::query::keyset::cursor_key_to_bind):
/// `id` / `corrects_id` render via [`Uuid::to_string`], `created_at` as
/// RFC 3339, `tenant_id` via its `Uuid` string, and the text columns
/// (`resource_id` / `resource_type` / `subject_id` / `subject_type` / `status`)
/// as-is. Returns `None` for an unknown field or a `NULL` optional column (a
/// `NULL` value can't seed a stable keyset boundary).
fn record_row_key(row: &UsageRecordRow, field: &str) -> Option<String> {
    match field {
        "id" => Some(row.id.to_string()),
        "corrects_id" => row.corrects_id.map(|id| id.to_string()),
        "created_at" => row.created_at.format(&Rfc3339).ok(),
        "tenant_id" => Some(row.tenant_id.to_string()),
        "resource_id" => Some(row.resource_id.clone()),
        "resource_type" => Some(row.resource_type.clone()),
        "subject_id" => row.subject_id.clone(),
        "subject_type" => row.subject_type.clone(),
        "status" => Some(row.status.clone()),
        _ => None,
    }
}

/// The dedup identity, mirroring the `usage_records_dedup_uniq` UNIQUE
/// `(tenant_id, gts_id, idempotency_key, created_at)`. The `created_at` component
/// is the microsecond count (see [`to_micros`]) so an in-memory key built from a
/// caller's `OffsetDateTime` matches the µs-truncated `created_at` Postgres
/// returns via `RETURNING` (timestamptz stores microseconds; sub-µs nanos do not
/// survive the round-trip).
type DedupKey = (Uuid, String, String, i128);

/// Build the [`DedupKey`] for an incoming record.
fn dedup_key(record: &UsageRecord) -> DedupKey {
    (
        record.tenant_id,
        gts_id_str(&record.gts_id).to_owned(),
        record.idempotency_key.as_str().to_owned(),
        to_micros(record.created_at),
    )
}

/// Build the [`DedupKey`] for a stored row, so an `INSERT … RETURNING` result
/// and an incoming record map to the same key (µs-normalized `created_at`).
fn row_dedup_key(row: &UsageRecordRow) -> DedupKey {
    (
        row.tenant_id,
        row.gts_id.clone(),
        row.idempotency_key.clone(),
        to_micros(row.created_at),
    )
}

/// Log a dedup-path invariant break (an `Internal`, "this should never happen"
/// condition) at `error` with the record's identifiers, then return the matching
/// [`UsageCollectorPluginError::Internal`]. Centralizing the log + build keeps
/// each silent break observable (DESIGN §Observability puts unbounded
/// identifiers in logs, not metric labels) without inflating the hot ingest
/// path's control flow.
fn dedup_invariant_break(record: &UsageRecord, msg: &'static str) -> UsageCollectorPluginError {
    tracing::error!(
        tenant_id = %record.tenant_id,
        gts_id = %gts_id_str(&record.gts_id),
        idempotency_key = %record.idempotency_key.as_str(),
        "{msg}"
    );
    UsageCollectorPluginError::internal(msg)
}

/// Log a retryable dedup-path transient at `warn` with the record's identifiers,
/// then return the matching [`UsageCollectorPluginError::Transient`]. The
/// degraded path is self-healing on retry but must still surface at `warn` so an
/// operator can see it (DESIGN §Observability).
fn dedup_transient(record: &UsageRecord, msg: &'static str) -> UsageCollectorPluginError {
    tracing::warn!(
        tenant_id = %record.tenant_id,
        gts_id = %gts_id_str(&record.gts_id),
        idempotency_key = %record.idempotency_key.as_str(),
        "{msg}"
    );
    UsageCollectorPluginError::transient(msg)
}

/// Deterministic plan for a batch insert.
///
/// `reps` are the first-occurrence representative records, one per distinct
/// dedup key, **sorted** by [`DedupKey`] so concurrent batches take the
/// 4-tuple-UNIQUE conflict locks in one global order (deadlock-free).
/// `first_index` maps each key to the input index of its first occurrence — the
/// only row that can win the slot; later same-key rows resolve against the
/// winner's record, exactly as the single-row path resolves a same-key hit.
struct BatchPlan<'a> {
    reps: Vec<&'a UsageRecord>,
    first_index: HashMap<DedupKey, usize>,
}

/// Collapse a batch to its distinct dedup keys (first occurrence wins),
/// sorted for a stable lock order. Pure — no DB. `reps` borrow from `records`,
/// which outlives the plan, so no record is cloned onto the plan.
fn plan_batch(records: &[UsageRecord]) -> BatchPlan<'_> {
    let mut first_index: HashMap<DedupKey, usize> = HashMap::new();
    let mut reps: Vec<(DedupKey, &UsageRecord)> = Vec::new();
    for (i, record) in records.iter().enumerate() {
        let key = dedup_key(record);
        if let std::collections::hash_map::Entry::Vacant(slot) = first_index.entry(key.clone()) {
            slot.insert(i);
            reps.push((key, record));
        }
    }
    reps.sort_by(|a, b| a.0.cmp(&b.0));
    BatchPlan {
        reps: reps.into_iter().map(|(_, r)| r).collect(),
        first_index,
    }
}

/// Total `create_batch` attempts: one initial try plus two retries. A bounded
/// in-process retry so a rare deadlock victim self-heals transparently instead
/// of bubbling an `Err(Transient)` to the host (see [`with_retry`]).
const MAX_BATCH_ATTEMPTS: u32 = 3;

/// Deterministic pre-jitter backoff base for the `attempt`-th retry (1-based).
/// A short exponential — 5 ms, 10 ms, … — because a deadlock victim can retry
/// almost immediately: the surviving transaction has already committed or
/// aborted by the time Postgres aborts the victim, so the contended dedup locks
/// are free. The shift is saturated so the schedule can never overflow
/// regardless of how `MAX_BATCH_ATTEMPTS` grows.
fn batch_retry_backoff_base(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(6);
    Duration::from_millis(5u64 << shift)
}

/// Full jitter: a uniformly random duration in `[0, upper]`. Decorrelates the
/// retry instants of batches that deadlocked on the same dedup locks, so they
/// do not all wake and re-contend at the same moment. An `upper` of at most
/// 1 ms is returned unchanged (nothing meaningful to spread).
fn full_jitter(upper: Duration) -> Duration {
    let upper_ms = u64::try_from(upper.as_millis()).unwrap_or(u64::MAX);
    if upper_ms <= 1 {
        return upper;
    }
    let jitter_ms = rand::rng().random_range(0..=upper_ms);
    Duration::from_millis(jitter_ms)
}

/// Backoff before the `attempt`-th retry (1-based: `batch_retry_backoff(1)`
/// precedes the first retry): the exponential [`batch_retry_backoff_base`] with
/// **full jitter** applied so concurrent deadlock victims spread across the
/// window instead of retrying in lockstep (thundering herd).
fn batch_retry_backoff(attempt: u32) -> Duration {
    full_jitter(batch_retry_backoff_base(attempt))
}

/// Retry predicate for [`with_retry`] around `create_batch`: retry **only** an
/// outer [`UsageCollectorPluginError::Transient`].
///
/// The deadlock victim surfaces as an outer `Transient` (the whole transaction
/// rolled back); serialization failures (`40001`) and connection blips collapse
/// to the same bucket inside the storage helpers, and all are safe to re-run
/// for this idempotent batch. `Internal`, `IdempotencyConflict`, and the typed
/// domain errors are non-retryable and returned unchanged. Per-row `Transient`
/// outcomes carried inside an `Ok(vec)` are deliberately not seen here — the
/// batch as a whole succeeded, so the loop never inspects them.
fn is_retryable_batch_error(err: &UsageCollectorPluginError) -> bool {
    matches!(err, UsageCollectorPluginError::Transient { .. })
}

/// Run `operation`, retrying while `should_retry` accepts its error, for up to
/// `max_attempts` total invocations; sleep `backoff(attempt)` before the
/// `attempt`-th retry. Returns the first `Ok`, or — once retries are exhausted
/// or the error is non-retryable — the last `Err` unchanged.
///
/// `on_retry(attempt, &err)` is invoked exactly once before each retry — after a
/// retryable failure and before the backoff sleep — with the failed 1-based
/// `attempt` number and the error it failed with. It is the observability seam
/// (log + retry counter): the combinator stays generic and DB-free, so the
/// caller supplies the tracing/metrics side effects. It never fires on a
/// first-attempt success or on a returned (non-retried) error, so a retry can be
/// told apart from a bubbled transient failure.
///
/// Generic and DB-free so the retry mechanics are unit-tested without a
/// transaction. `operation` is an `Fn` invoked fresh each attempt (it borrows
/// the caller's input, so re-invocation is allocation-free), which is exactly
/// the right unit of retry for `create_batch_inner`: every attempt acquires a
/// fresh connection and opens a fresh transaction. There is zero happy-path
/// cost — on success the loop runs the operation once and neither sleeps,
/// allocates a backoff, nor calls `on_retry`.
async fn with_retry<T, E, Op, Fut>(
    max_attempts: u32,
    backoff: impl Fn(u32) -> Duration,
    should_retry: impl Fn(&E) -> bool,
    on_retry: impl Fn(u32, &E),
    operation: Op,
) -> Result<T, E>
where
    Op: Fn() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    let mut attempt: u32 = 1;
    loop {
        match operation().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt >= max_attempts || !should_retry(&err) {
                    return Err(err);
                }
                on_retry(attempt, &err);
                // `toolkit::tokio` is the crate's tokio re-export (matching
                // `toolkit::tokio::spawn` / `select!` elsewhere in this gear);
                // `tokio` itself is only a dev-dependency.
                toolkit::tokio::time::sleep(backoff(attempt)).await;
                attempt += 1;
            }
        }
    }
}

/// Outcome of reading the existing `usage_records` row for a not-won key.
enum ConflictRead {
    /// The conflicting row exists — resolve absorb vs conflict against it.
    Stored(Box<UsageRecordRow>),
    /// The conflicting row's chunk was dropped by retention between the
    /// conflicting insert and the read → retryable `Transient`.
    Stale,
}

/// `OffsetDateTime` at microsecond precision, as the unix-epoch microsecond
/// count.
///
/// Postgres `timestamptz` stores microseconds; an incoming `OffsetDateTime`
/// may carry sub-microsecond nanos that never survive the round-trip. Comparing
/// the microsecond counts makes the canonical-equality check agree with what
/// the DB actually persisted.
///
/// Delegates to the SDK's [`usage_collector_sdk::created_at_micros`] so this
/// dedup-equality projection and the identity derivation in
/// [`usage_collector_sdk::derive_usage_record_id`] share one canonical µs
/// primitive — a precision change in one crate cannot silently diverge them.
fn to_micros(dt: OffsetDateTime) -> i128 {
    usage_collector_sdk::created_at_micros(dt)
}

/// Compare the caller-supplied canonical fields of a stored row against an
/// incoming record (§3.6: absorb vs conflict).
///
/// The canonical set compared here is `id`, `value`, `resource_ref`,
/// `subject_ref`, `corrects_id`, and `metadata`. Excluded are the dedup-key
/// fields (`tenant_id` / `gts_id` / `idempotency_key` / `created_at`) — the
/// lookup key, already matched — and server-managed `status` / `ingested_at`.
/// `metadata` is compared after decoding the stored `jsonb` back to the typed
/// map.
///
/// NOTE — `created_at` is excluded because it is part of the dedup key (the
/// `(tenant_id, gts_id, idempotency_key, created_at)` 4-tuple UNIQUE): this
/// function only runs once that key has already matched, so the timestamps are
/// equal by construction. A same-`idempotency_key` request carrying a
/// *different* `created_at` is a distinct 4-tuple — a distinct record with a
/// distinct `id` (ADR-0014 makes `created_at` part of the record identity), not
/// a conflict; see DESIGN.md §2.2.
///
/// NOTE — the record `id` is compared here (stored `id` column vs the
/// incoming record's `id`). Since `id` is a deterministic projection of the
/// 4-tuple dedup key `(tenant_id, gts_id, idempotency_key, created_at)`
/// (ADR-0014) and this function only runs once that full key has matched, the
/// two ids are equal by construction, so this comparison is a defensive
/// tautology rather than a fail-closed guard against a mismatched
/// caller-supplied identity. It is kept so a future non-deterministic-id path
/// (or a corrupted stored row) still surfaces as an `IdempotencyConflict`
/// rather than a silent absorb.
///
/// Comparing `id` here is explicitly sanctioned by the SPI contract:
/// plugin-spi.md §"Plugin-specific outputs" (Create single record output) and
/// domain-model.md §2.5 `IdempotencyKey` exclude only the server-managed
/// `status` from the caller-canonical comparison, and both note that a plugin
/// MAY defensively verify the deterministic `id` against the derived value —
/// surfacing a corrupted stored row as `IdempotencyConflict` rather than a
/// silent absorb — without changing the outcome for well-formed data. That is
/// exactly the guard described above.
///
/// The one edge the tautology relies on: a pre-`id`-determinism row would break
/// it — its stored `id` is the old *caller-supplied* `uuid`, not the derived
/// value — surfacing an exact retry as a false `IdempotencyConflict`.
/// Deployments are greenfield, so no such row exists; see migration
/// `0002_rename_uuid_to_id.sql`.
///
/// # Errors
///
/// Returns [`UsageCollectorPluginError::Internal`] when the stored `metadata`
/// `jsonb` cannot be decoded back to the typed map — a stored-data invariant
/// break, distinct from a canonical-field mismatch (which returns `Ok(false)`).
fn canonical_equal(
    row: &UsageRecordRow,
    incoming: &UsageRecord,
) -> Result<bool, UsageCollectorPluginError> {
    let stored_metadata = metadata_jsonb_to_map(row.metadata.clone())?;
    Ok(row.id == incoming.id
        && row.value == incoming.value
        && row.resource_id == incoming.resource_ref.resource_id()
        && row.resource_type == incoming.resource_ref.resource_type()
        && row.subject_id.as_deref()
            == incoming
                .subject_ref
                .as_ref()
                .map(usage_collector_sdk::SubjectRef::subject_id)
        && row.subject_type.as_deref()
            == incoming.subject_ref.as_ref().and_then(|s| s.subject_type())
        && row.corrects_id == incoming.corrects_id
        && stored_metadata == incoming.metadata)
}

#[async_trait]
impl RecordStore for PgRecordStore {
    // @cpt-flow:cpt-cf-uc-plugin-seq-ingest-dedup:p2
    async fn create(&self, record: UsageRecord) -> Result<UsageRecord, UsageCollectorPluginError> {
        // Time the whole single-row call; the per-row counters live in
        // `create_inner` so they count once regardless of single-vs-batch.
        let t = Instant::now();
        let result = self.create_inner(record).await;
        self.metrics
            .record_insert(InsertMode::Single, t.elapsed().as_secs_f64());
        result
    }

    // @cpt-flow:cpt-cf-uc-plugin-seq-ingest-batch:p2
    async fn create_batch(
        &self,
        records: Vec<UsageRecord>,
    ) -> Result<Vec<Result<UsageRecord, UsageCollectorPluginError>>, UsageCollectorPluginError>
    {
        if records.is_empty() {
            tracing::warn!(
                "create_usage_records called with an empty batch (host-contract breach)"
            );
            return Err(UsageCollectorPluginError::internal(
                "create_usage_records called with an empty batch (host-contract breach)",
            ));
        }

        // Per-row dedup semantics, input order, and per-row metrics are
        // preserved by `create_batch_inner`; the multi-row write replaces the
        // former N+1 per-row loop (DESIGN cpt-cf-uc-plugin-seq-ingest-batch).
        //
        // Wrap the whole call in a bounded retry: on an outer `Transient` (the
        // classic ABBA deadlock victim aborted as `40P01`, a serialization
        // failure `40001`, or a connection blip) re-run the operation up to
        // `MAX_BATCH_ATTEMPTS` times. Each attempt acquires a fresh connection
        // and opens a fresh transaction (`create_batch_inner` does both), so a
        // rolled-back attempt leaves no state behind. Re-running is safe: the
        // transaction is atomic and the dedup keys make it idempotent, so a
        // re-run either re-claims the same slots or absorbs/conflicts against
        // the now-committed survivor. `Ok(vec)` is never retried — per-row
        // `Transient` outcomes inside it are the host's to handle (the batch as
        // a whole succeeded), and retrying them would be a correctness bug.
        let n = records.len();
        // Time the whole operation including any retries, recorded once
        // regardless of outcome (matches the single-call behaviour). On the
        // happy path the loop runs `create_batch_inner` exactly once.
        let t = Instant::now();
        let result = with_retry(
            MAX_BATCH_ATTEMPTS,
            batch_retry_backoff,
            is_retryable_batch_error,
            |attempt, err| {
                // Make the retry observable: a distinct warn + counter so a
                // self-healed deadlock victim can be told apart from a returned
                // transient error (which only moves the backend-error counter).
                tracing::warn!(
                    attempt,
                    max_attempts = MAX_BATCH_ATTEMPTS,
                    error = %err,
                    "retrying usage-record batch write after transient backend error"
                );
                self.metrics.inc_batch_retry();
            },
            || self.create_batch_inner(&records),
        )
        .await;
        self.metrics
            .record_insert(InsertMode::Batch, t.elapsed().as_secs_f64());
        if result.is_ok() {
            // `n` is a row count: convert via `try_from` (no `as` cast),
            // saturating an implausibly huge batch to `u32::MAX` before f64.
            self.metrics
                .record_batch_rows(f64::from(u32::try_from(n).unwrap_or(u32::MAX)));
        }
        result
    }

    async fn get(&self, id: Uuid) -> Result<UsageRecord, UsageCollectorPluginError> {
        // Lookup by the public `id`. This relies on a one-record-per-`id`
        // contract, which the hypertable schema cannot enforce on its own — a
        // `UNIQUE` there must include the `created_at` partition key, so only the
        // composite PK `(id, created_at)` is enforced. `fetch_optional` therefore
        // returns the first matching row.
        //
        // `id` is a `UUIDv5` of the full 4-tuple dedup key
        // `(tenant_id, gts_id, idempotency_key, created_at)` (ADR-0014), which is
        // exactly this plugin's dedup identity, so each stored row carries a
        // distinct `id`: `WHERE id = $1` matches at most one row. (Before
        // ADR-0014 `id` excluded `created_at`, so the same 3-tuple at two
        // `created_at` values shared one `id`; folding `created_at` into the
        // derivation closed that collision — see DESIGN.md §2.2.)
        let sql = format!("SELECT {RECORD_COLUMNS} FROM usage_records WHERE id = $1");
        let mut conn = self.timed_acquire().await?;
        let row = sqlx::query_as::<_, UsageRecordRow>(&sql)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|err| self.record_backend_error(&err))?;

        match row {
            Some(row) => record_row_to_model(row),
            None => Err(UsageCollectorPluginError::UsageRecordNotFound { id }),
        }
    }

    /// Keyset-paginated `usage_records` list, scoped to `gts_id` (bound at
    /// `$1`), with optional `$filter`, metadata side-channel filters, and a
    /// cursor.
    ///
    /// Builds `SELECT {RECORD_COLUMNS} FROM usage_records WHERE gts_id = $1
    /// [AND <filter>] [AND <metadata>] [AND <keyset>] ORDER BY <order> LIMIT
    /// <n+1>`. The extra `+1` row is the look-ahead that detects a following
    /// page; it is truncated before mapping. All identifiers come from the
    /// [`record_column`] allowlist and the static [`RECORD_COLUMNS`]; every
    /// value is bound (`$N`).
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorPluginError::Internal`] when the filter AST
    /// references an unknown field, the cursor's filter hash disagrees with
    /// `query.filter_hash`, an order/keyset field is off the allowlist, a stored
    /// row cannot be mapped, or the DB query fails.
    // @cpt-flow:cpt-cf-uc-plugin-seq-list-keyset:p2
    async fn list(
        &self,
        gts_id: UsageTypeGtsId,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
    ) -> Result<ODataPage<UsageRecord>, UsageCollectorPluginError> {
        // Time the full raw-list call and count the request. The drop-timer
        // records the histogram on every return — including the validation
        // error arms below — not just on success.
        let _timer =
            OpDurationGuard::start(Arc::clone(&self.metrics), TimedOp::Query(QueryKind::Raw));
        self.metrics.inc_query_request(QueryKind::Raw);
        // Defense-in-depth: clamp the caller's `$top` to `MAX_PAGE_SIZE` so a
        // value that slipped past the core gateway's `$top` cap can never drive
        // an unbounded `LIMIT n+1 ... fetch_all`.
        let limit = effective_page_size(query.limit, DEFAULT_PAGE_SIZE);

        // `$1` is reserved for the `gts_id` scope bind; every translated bind
        // therefore starts at `$2`.
        let mut ctx = SqlCtx::new(2);
        let mut clauses: Vec<String> = vec!["gts_id = $1".to_owned()];

        // `$filter` (validated AST -> typed node -> parameterized fragment).
        if let Some(expr) = query.filter() {
            let node = convert_expr_to_filter_node::<UsageRecordFilterField>(expr)
                .map_err(|e| UsageCollectorPluginError::internal(format!("invalid filter: {e}")))?;
            let fragment = translate_record_filter(&node, &mut ctx)
                .map_err(UsageCollectorPluginError::internal)?;
            clauses.push(fragment);
        }

        // Metadata side-channel: AND across filters, OR within one filter's
        // values (see [`push_metadata_filter_clauses`]).
        push_metadata_filter_clauses(metadata_filter, &mut ctx, &mut clauses);

        // Keyset continuation (forward only). The cursor's filter hash must
        // match the live query's so a cursor is never replayed against a
        // different filter.
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
            // The cursor's keys (`cursor.k`) are positional, bound against the
            // live `query.order` columns below. If the order changed between
            // pages at the same arity, old keys would bind to new columns —
            // silently wrong pagination. The cursor carries the signed sort
            // tokens (`cursor.s`) precisely to detect this, mirroring the
            // filter-hash guard above.
            if !query.order.equals_signed_tokens(&cursor.s) {
                return Err(UsageCollectorPluginError::internal(
                    "cursor sort order mismatch",
                ));
            }
            let order_pairs: Vec<(&str, bool)> = query
                .order
                .0
                .iter()
                .map(|key| (key.field.as_str(), matches!(key.dir, SortDir::Asc)))
                .collect();
            let predicate = keyset_predicate(
                &order_pairs,
                &cursor.k,
                record_column,
                |name| UsageRecordFilterField::from_name(name).map(|f| f.kind()),
                is_keyset_safe_record_field,
                &mut ctx,
            )
            .map_err(UsageCollectorPluginError::internal)?;
            clauses.push(predicate);
        }

        let order_sql = render_order_by(&query.order, record_column)
            .map_err(UsageCollectorPluginError::internal)?;

        let sql = format!(
            "SELECT {RECORD_COLUMNS} FROM usage_records WHERE {} ORDER BY {order_sql} LIMIT {}",
            clauses.join(" AND "),
            limit.saturating_add(1),
        );

        let mut q = sqlx::query_as::<_, UsageRecordRow>(&sql).bind(gts_id_str(&gts_id));
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
            let keys = query
                .order
                .0
                .iter()
                .map(|key| {
                    record_row_key(last, &key.field).ok_or_else(|| {
                        UsageCollectorPluginError::internal(format!(
                            "order field `{}` has no cursor key on the row",
                            key.field
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let token = encode_next_cursor(&query.order, &keys, query.filter_hash.as_deref())
                .map_err(UsageCollectorPluginError::internal)?;
            Some(token)
        } else {
            None
        };

        let items = rows
            .into_iter()
            .map(record_row_to_model)
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

    /// Pushed-down aggregation over `usage_records`, scoped to `gts_id` (bound
    /// at `$1`) and to `status = 'active'`, with optional `$filter`, metadata
    /// side-channel filters, and a `GROUP BY` over the spec's dimensions
    /// (§3.6 aggregated query).
    ///
    /// Builds `SELECT <dim exprs…>, <AGG> FROM usage_records WHERE gts_id = $1
    /// AND status = 'active' [AND corrects_id IS NULL] [AND <filter>] [AND
    /// <metadata>] [AND <subject-not-null guards>] [GROUP BY 1, 2, …]`. The
    /// aggregate ([`agg_select_expr`]) and each dimension
    /// ([`dimension_select_expr`]) come from closed enum allowlists; the only
    /// caller-derived values (a grouped metadata key, `$filter` operands,
    /// metadata side-channel values) are bound (`$N`). `status = 'active'` is
    /// always applied. The `corrects_id IS NULL` partition is op-dependent
    /// ([`corrects_id_partition_clause`]): `SUM` nets across all active rows —
    /// compensations carry a signed `value` — so it omits the partition; every
    /// other op (`COUNT`/`MIN`/`MAX`/`AVG`) restricts to `corrects_id IS NULL`
    /// rows, since compensations adjust `SUM` and are not events (plugin-spi.md
    /// §Method 3). With an empty `group_by` there is no `GROUP BY` clause, so
    /// the query yields exactly one bucket with `key = []`.
    ///
    /// Each returned row maps to one [`AggregationBucket`]: the `k` dimension
    /// columns read positionally as `Option<String>` (a `NULL` dimension
    /// becomes the empty string — relevant only when a grouped metadata key is
    /// absent on some active rows), and the aggregate at index `k` reads as
    /// `Option<BigDecimal>` (arbitrary precision, carried through as-is;
    /// `NULL` -> `None`).
    ///
    /// # Errors
    ///
    /// Returns [`UsageCollectorPluginError::Internal`] when the filter AST
    /// references an unknown field or is otherwise invalid, the DB query fails,
    /// or a result column cannot be read at its expected type.
    // @cpt-flow:cpt-cf-uc-plugin-seq-query-aggregated:p2
    async fn aggregate(
        &self,
        gts_id: UsageTypeGtsId,
        query: &ODataQuery,
        metadata_filter: &[MetadataFilter],
        spec: AggregationSpec,
    ) -> Result<AggregationResult, UsageCollectorPluginError> {
        // Time the full aggregated-query call and count the request. The
        // drop-timer records the histogram on every return, not just success.
        let _timer = OpDurationGuard::start(
            Arc::clone(&self.metrics),
            TimedOp::Query(QueryKind::Aggregated),
        );
        self.metrics.inc_query_request(QueryKind::Aggregated);
        // `$1` is reserved for the `gts_id` scope bind; every translated bind
        // therefore starts at `$2`.
        let mut ctx = SqlCtx::new(2);
        let mut clauses: Vec<String> =
            vec!["gts_id = $1".to_owned(), "status = 'active'".to_owned()];

        // `corrects_id` partition (plugin-spi.md §Method 3): `SUM` nets across
        // all active rows (compensations carry a signed `value`); every other op
        // operates over `corrects_id IS NULL` rows only, since compensations
        // adjust `SUM` and are not events. Load-bearing for `COUNT`-on-counter.
        if let Some(clause) = corrects_id_partition_clause(spec.op) {
            clauses.push(clause.to_owned());
        }

        // `$filter` (validated AST -> typed node -> parameterized fragment).
        if let Some(expr) = query.filter() {
            let node = convert_expr_to_filter_node::<UsageRecordFilterField>(expr)
                .map_err(|e| UsageCollectorPluginError::internal(format!("invalid filter: {e}")))?;
            let fragment = translate_record_filter(&node, &mut ctx)
                .map_err(UsageCollectorPluginError::internal)?;
            clauses.push(fragment);
        }

        // Metadata side-channel (same expansion as `list`).
        push_metadata_filter_clauses(metadata_filter, &mut ctx, &mut clauses);

        // Build dimension SELECT exprs in GROUP-BY order, binding any metadata
        // keys, and emit subject-not-null guards so subject-less rows are
        // excluded from subject grouping (per the SDK dimension docs).
        let mut select_dims: Vec<String> = Vec::with_capacity(spec.group_by.len());
        for dim in &spec.group_by {
            match dim {
                AggregationDimension::SubjectId => {
                    clauses.push("subject_id IS NOT NULL".to_owned());
                }
                AggregationDimension::SubjectType => {
                    clauses.push("subject_type IS NOT NULL".to_owned());
                }
                _ => {}
            }
            select_dims.push(dimension_select_expr(dim, &mut ctx));
        }

        // SELECT list = dimension exprs ++ the aggregate. With no dimensions
        // the SELECT is just the aggregate (single-bucket / no-grouping case).
        let dim_count = select_dims.len();
        let mut select_parts = select_dims;
        select_parts.push(agg_select_expr(spec.op).to_owned());
        let select_list = select_parts.join(", ");

        // GROUP BY by ordinal (1..=k) so the bound metadata expr is not
        // repeated; omitted entirely when there are no dimensions.
        let group_by = if dim_count == 0 {
            String::new()
        } else {
            let ordinals = (1..=dim_count)
                .map(|n| n.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            format!(" GROUP BY {ordinals}")
        };

        // Bound the distinct-group cardinality so a high-cardinality `group_by`
        // cannot materialize an unbounded bucket set into memory; the gateway
        // rejects an over-cap result (plugin-spi.md §Method 3).
        let limit_clause = aggregate_limit_clause(dim_count);
        let sql = format!(
            "SELECT {select_list} FROM usage_records WHERE {}{group_by}{limit_clause}",
            clauses.join(" AND "),
        );

        let mut q = sqlx::query(&sql).bind(gts_id_str(&gts_id));
        for b in &ctx.binds {
            q = bind_one_query(q, b);
        }
        let mut conn = self.timed_acquire().await?;
        let rows = q
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| self.record_backend_error(&e))?;

        let mut buckets = Vec::with_capacity(rows.len());
        for row in rows {
            let mut key = Vec::with_capacity(dim_count);
            for i in 0..dim_count {
                let dim = row.try_get::<Option<String>, _>(i).map_err(|e| {
                    UsageCollectorPluginError::internal(format!(
                        "aggregate dimension column {i} read failed: {e}"
                    ))
                })?;
                // A NULL dimension (e.g. a grouped metadata key absent on some
                // active rows) becomes the empty string in the bucket key.
                key.push(dim.unwrap_or_default());
            }
            let value = row
                .try_get::<Option<BigDecimal>, _>(dim_count)
                .map_err(|e| {
                    UsageCollectorPluginError::internal(format!(
                        "aggregate value column {dim_count} read failed: {e}"
                    ))
                })?;
            buckets.push(AggregationBucket { key, value });
        }

        // `_timer` records `query.duration` on drop (success and error alike).
        Ok(AggregationResult { buckets })
    }

    /// Deactivate a record and its depth-1 active compensations in one
    /// transaction (§3.6 deactivate-cascade).
    ///
    /// Locks the target row `FOR UPDATE` and reads its `status`: a missing row
    /// is `UsageRecordNotFound`, an already-`inactive` row is
    /// `UsageRecordAlreadyInactive`. An `active` target and every `active` row
    /// whose `corrects_id` points at it (depth-1 only) flip to `inactive` in a
    /// single `UPDATE`. The transition is one-way and mutates no other column;
    /// rows already `inactive` and unrelated rows are untouched.
    ///
    /// `WHERE id = $1` addresses one logical record. The schema does not carry a
    /// plain `UNIQUE (id)` (a hypertable UNIQUE must include the `created_at`
    /// partition key, so only the composite PK `(id, created_at)` and the dedup
    /// `(tenant_id, gts_id, idempotency_key, created_at)` UNIQUE exist), but `id`
    /// is a `UUIDv5` of that full 4-tuple (ADR-0014), so each stored row carries
    /// a distinct `id` and this `UPDATE` flips at most one row.
    // @cpt-flow:cpt-cf-uc-plugin-seq-deactivate-cascade:p2
    async fn deactivate(&self, id: Uuid) -> Result<(), UsageCollectorPluginError> {
        // Time the full deactivation cascade; the drop-timer records the
        // duration on every return — including the not-found / already-inactive
        // and error arms — not just on a successful commit.
        let _timer = OpDurationGuard::start(Arc::clone(&self.metrics), TimedOp::Deactivate);
        let mut conn = self.timed_acquire().await?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| self.record_backend_error(&e))?;

        // Lock + read the target's status. Since `id` is a `UUIDv5` of the full
        // 4-tuple dedup key including `created_at` (ADR-0014), each stored row
        // carries a distinct `id`, so this locks the single addressed row and
        // the `WHERE id = $1` UPDATE below flips exactly that row.
        let status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM usage_records WHERE id = $1 FOR UPDATE",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| self.record_backend_error(&e))?;

        match status {
            None => {
                tx.rollback().await.ok();
                return Err(UsageCollectorPluginError::UsageRecordNotFound { id });
            }
            Some(s) if s == "inactive" => {
                tx.rollback().await.ok();
                return Err(UsageCollectorPluginError::UsageRecordAlreadyInactive { id });
            }
            Some(_) => {}
        }

        // Flip the target and its depth-1 active compensations. One-way; the
        // `status = 'active'` guard on the compensations keeps already-inactive
        // children untouched and bounds the cascade to a single level.
        sqlx::query(
            "UPDATE usage_records SET status = 'inactive' \
             WHERE id = $1 OR (corrects_id = $1 AND status = 'active')",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|e| self.record_backend_error(&e))?;

        tx.commit()
            .await
            .map_err(|e| self.record_backend_error(&e))?;
        // `_timer` records `deactivate.duration` on drop.
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "record_store_tests.rs"]
mod record_store_tests;
