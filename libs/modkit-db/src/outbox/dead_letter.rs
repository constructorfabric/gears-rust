//! # Dead letters
//!
//! Dead letters hold messages that a handler explicitly rejected via
//! [`HandlerResult::Reject`]. They are an **exceptional recovery mechanism**,
//! not a routine processing path.
//!
//! **If you find yourself replaying dead letters frequently, the handler logic
//! needs fixing — not the dead letter infrastructure.**
//!
//! ## Status lifecycle
//!
//! ```text
//!                   replay()                       resolve()
//!   pending ──────────────────► reprocessing ──────────────────► resolved
//!      │  ▲                          │
//!      │  │  reject()                │  reject()
//!      │  └──────────────────────────┘
//!      │
//!      │  discard()
//!      └───────────────────────────────────────────────────────► discarded
//! ```
//!
//! ## Operations
//!
//! | Operation  | Purpose | Parameter | Concurrency |
//! |------------|---------|-----------|-------------|
//! | `list`     | Inspect dead letters | `&DeadLetterFilter` | Safe |
//! | `count`    | Count matching | `&DeadLetterFilter` | Safe |
//! | `replay`   | Claim for reprocessing | `&DeadLetterScope` + `Duration` | Row-locked |
//! | `resolve`  | Mark as resolved | `&[i64]` | Safe |
//! | `reject`   | Return to pending | `&[i64]` + reason | Safe |
//! | `discard`  | Soft-delete | `&DeadLetterScope` | Row-locked |
//! | `cleanup`  | Delete terminal states | `&DeadLetterScope` | Safe |

use std::fmt::Write as _;
use std::time::Duration;

use sea_orm::{
    ConnectionTrait, DbBackend, FromQueryResult, Statement, TransactionTrait, TryGetError,
    TryGetable,
};

use super::types::OutboxError;
use crate::secure::SeaOrmRunner;

/// Default row limit applied to `dead_letter_list` when no explicit limit is set.
const DEFAULT_DEAD_LETTER_LIMIT: u32 = 100;

/// Dead letter lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadLetterStatus {
    Pending,
    Reprocessing,
    Resolved,
    Discarded,
}

impl std::fmt::Display for DeadLetterStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => f.write_str("pending"),
            Self::Reprocessing => f.write_str("reprocessing"),
            Self::Resolved => f.write_str("resolved"),
            Self::Discarded => f.write_str("discarded"),
        }
    }
}

impl std::str::FromStr for DeadLetterStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "reprocessing" => Ok(Self::Reprocessing),
            "resolved" => Ok(Self::Resolved),
            "discarded" => Ok(Self::Discarded),
            other => Err(format!("invalid dead letter status: {other}")),
        }
    }
}

impl TryGetable for DeadLetterStatus {
    fn try_get_by<I: sea_orm::ColIdx>(
        res: &sea_orm::QueryResult,
        idx: I,
    ) -> Result<Self, TryGetError> {
        let val: String = res.try_get_by(idx)?;
        val.parse()
            .map_err(|e: String| TryGetError::DbErr(sea_orm::DbErr::Type(e)))
    }
}

fn runner_backend(runner: &SeaOrmRunner<'_>) -> DbBackend {
    match runner {
        SeaOrmRunner::Conn(c) => c.get_database_backend(),
        SeaOrmRunner::Tx(t) => t.get_database_backend(),
    }
}

/// A dead-lettered message with self-contained payload.
#[derive(Debug, FromQueryResult)]
pub struct DeadLetterMessage {
    pub id: i64,
    pub partition_id: i64,
    pub seq: i64,
    pub payload: Vec<u8>,
    pub payload_type: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub failed_at: chrono::DateTime<chrono::Utc>,
    pub last_error: Option<String>,
    pub attempts: i16,
    pub status: DeadLetterStatus,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub deadline: Option<chrono::DateTime<chrono::Utc>>,
}

/// Row-targeting scope shared by all dead letter operations.
///
/// Used by `replay`, `discard`, `cleanup`. Does NOT include `status` — those
/// operations hardcode their own status logic.
#[derive(Debug, Default)]
pub struct DeadLetterScope {
    pub partition_id: Option<i64>,
    pub queue: Option<String>,
    pub payload_type: Option<String>,
    pub limit: Option<u32>,
}

impl DeadLetterScope {
    #[must_use]
    pub fn partition(mut self, id: i64) -> Self {
        self.partition_id = Some(id);
        self
    }

    #[must_use]
    pub fn queue(mut self, queue: impl Into<String>) -> Self {
        self.queue = Some(queue.into());
        self
    }

    #[must_use]
    pub fn payload_type(mut self, pt: impl Into<String>) -> Self {
        self.payload_type = Some(pt.into());
        self
    }

    #[must_use]
    pub fn limit(mut self, n: u32) -> Self {
        self.limit = Some(n);
        self
    }
}

/// Full filter for querying dead letters. Used by `list` and `count`.
///
/// Default: `status = Some(Pending)`, empty scope.
pub struct DeadLetterFilter {
    pub scope: DeadLetterScope,
    pub status: Option<DeadLetterStatus>,
}

impl DeadLetterFilter {
    /// Start from a scope, defaulting to `status = Some(Pending)`.
    #[must_use]
    pub fn from_scope(scope: DeadLetterScope) -> Self {
        Self {
            scope,
            status: Some(DeadLetterStatus::Pending),
        }
    }

    #[must_use]
    pub fn partition(mut self, id: i64) -> Self {
        self.scope.partition_id = Some(id);
        self
    }

    #[must_use]
    pub fn queue(mut self, queue: impl Into<String>) -> Self {
        self.scope.queue = Some(queue.into());
        self
    }

    #[must_use]
    pub fn payload_type(mut self, pt: impl Into<String>) -> Self {
        self.scope.payload_type = Some(pt.into());
        self
    }

    #[must_use]
    pub fn limit(mut self, n: u32) -> Self {
        self.scope.limit = Some(n);
        self
    }

    #[must_use]
    pub fn status(mut self, status: DeadLetterStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Match all statuses (no status filter).
    #[must_use]
    pub fn any_status(mut self) -> Self {
        self.status = None;
        self
    }
}

impl Default for DeadLetterFilter {
    fn default() -> Self {
        Self {
            scope: DeadLetterScope::default(),
            status: Some(DeadLetterStatus::Pending),
        }
    }
}

/// List dead-lettered messages with optional filtering.
pub(super) async fn dead_letter_list(
    runner: SeaOrmRunner<'_>,
    filter: &DeadLetterFilter,
) -> Result<Vec<DeadLetterMessage>, OutboxError> {
    let backend = runner_backend(&runner);
    let (sql, values) = build_select_query(backend, filter);
    let stmt = Statement::from_sql_and_values(backend, &sql, values);

    let rows = match &runner {
        SeaOrmRunner::Conn(c) => DeadLetterMessage::find_by_statement(stmt).all(*c).await?,
        SeaOrmRunner::Tx(t) => DeadLetterMessage::find_by_statement(stmt).all(*t).await?,
    };
    Ok(rows)
}

/// Count dead-lettered messages matching the filter.
pub(super) async fn dead_letter_count(
    runner: SeaOrmRunner<'_>,
    filter: &DeadLetterFilter,
) -> Result<u64, OutboxError> {
    #[derive(Debug, FromQueryResult)]
    struct Count {
        cnt: i64,
    }

    let backend = runner_backend(&runner);
    let (sql, values) = build_count_query(backend, filter);
    let stmt = Statement::from_sql_and_values(backend, &sql, values);

    let row = match &runner {
        SeaOrmRunner::Conn(c) => Count::find_by_statement(stmt).one(*c).await?,
        SeaOrmRunner::Tx(t) => Count::find_by_statement(stmt).one(*t).await?,
    };

    #[allow(clippy::cast_sign_loss)]
    Ok(row.map_or(0, |r| r.cnt as u64))
}

/// Claim dead letters for reprocessing. Transitions `pending → reprocessing`
/// and reclaims orphaned `reprocessing` rows whose deadline has expired.
///
/// Returns the claimed messages. The caller decides what to do with them —
/// re-enqueue, process inline, forward elsewhere. Use `dead_letter_resolve()`
/// on success or `dead_letter_reject()` on failure.
///
/// On Postgres and `MySQL`, uses `FOR UPDATE SKIP LOCKED` to prevent concurrent
/// callers from claiming the same rows.
pub(super) async fn dead_letter_replay(
    runner: SeaOrmRunner<'_>,
    scope: &DeadLetterScope,
    timeout: Duration,
) -> Result<Vec<DeadLetterMessage>, OutboxError> {
    let backend = runner_backend(&runner);

    let txn = match &runner {
        SeaOrmRunner::Conn(c) => c.begin().await?,
        SeaOrmRunner::Tx(t) => t.begin().await?,
    };

    let (sql, values) = build_replay_select(backend, scope);
    let rows =
        DeadLetterMessage::find_by_statement(Statement::from_sql_and_values(backend, &sql, values))
            .all(&txn)
            .await?;

    if rows.is_empty() {
        txn.commit().await?;
        return Ok(Vec::new());
    }

    let dl_ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let claim_sql = build_batch_claim(backend, dl_ids.len());
    let mut claim_values: Vec<sea_orm::Value> = Vec::with_capacity(1 + dl_ids.len());
    #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
    claim_values.push((timeout.as_secs() as i64).into());
    for &id in &dl_ids {
        claim_values.push(id.into());
    }
    txn.execute(Statement::from_sql_and_values(
        backend,
        &claim_sql,
        claim_values,
    ))
    .await?;

    txn.commit().await?;
    Ok(rows)
}

/// Transition `reprocessing → resolved`. Called after successfully handling
/// claimed dead letters.
///
/// Returns count of resolved rows. Rows not in `reprocessing` state are
/// skipped (returns 0 for those).
pub(super) async fn dead_letter_resolve(
    runner: SeaOrmRunner<'_>,
    ids: &[i64],
) -> Result<u64, OutboxError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let backend = runner_backend(&runner);
    let sql = build_batch_resolve(backend, ids.len());
    let values: Vec<sea_orm::Value> = ids.iter().map(|&id| id.into()).collect();
    let stmt = Statement::from_sql_and_values(backend, &sql, values);
    let result = match &runner {
        SeaOrmRunner::Conn(c) => c.execute(stmt).await?,
        SeaOrmRunner::Tx(t) => t.execute(stmt).await?,
    };
    Ok(result.rows_affected())
}

/// Transition `reprocessing → pending` with attempts++, `failed_at` refreshed,
/// `last_error` updated. Called when handling a claimed dead letter fails.
///
/// Returns count of rejected rows.
pub(super) async fn dead_letter_reject(
    runner: SeaOrmRunner<'_>,
    ids: &[i64],
    reason: &str,
) -> Result<u64, OutboxError> {
    if ids.is_empty() {
        return Ok(0);
    }
    let backend = runner_backend(&runner);
    let sql = build_batch_reject(backend, ids.len());
    let mut values: Vec<sea_orm::Value> = Vec::with_capacity(1 + ids.len());
    values.push(reason.to_owned().into());
    for &id in ids {
        values.push(id.into());
    }
    let stmt = Statement::from_sql_and_values(backend, &sql, values);
    let result = match &runner {
        SeaOrmRunner::Conn(c) => c.execute(stmt).await?,
        SeaOrmRunner::Tx(t) => t.execute(stmt).await?,
    };
    Ok(result.rows_affected())
}

/// Discard pending dead letters — transitions `pending → discarded`.
///
/// Uses `FOR UPDATE SKIP LOCKED` for concurrent safety.
pub(super) async fn dead_letter_discard(
    runner: SeaOrmRunner<'_>,
    scope: &DeadLetterScope,
) -> Result<u64, OutboxError> {
    #[derive(Debug, FromQueryResult)]
    struct Id {
        id: i64,
    }

    let backend = runner_backend(&runner);

    let txn = match &runner {
        SeaOrmRunner::Conn(c) => c.begin().await?,
        SeaOrmRunner::Tx(t) => t.begin().await?,
    };

    let (sql, values) = build_discard_select(backend, scope);

    let rows = Id::find_by_statement(Statement::from_sql_and_values(backend, &sql, values))
        .all(&txn)
        .await?;

    if rows.is_empty() {
        txn.commit().await?;
        return Ok(0);
    }

    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let update_sql = build_batch_discard(backend, ids.len());
    let update_values: Vec<sea_orm::Value> = ids.iter().map(|&id| id.into()).collect();
    txn.execute(Statement::from_sql_and_values(
        backend,
        &update_sql,
        update_values,
    ))
    .await?;

    txn.commit().await?;

    #[allow(clippy::cast_possible_truncation)]
    Ok(ids.len() as u64)
}

/// Delete terminal-state dead letters (`resolved` + `discarded`).
///
/// `pending` and `reprocessing` rows are always preserved.
pub(super) async fn dead_letter_cleanup(
    runner: SeaOrmRunner<'_>,
    scope: &DeadLetterScope,
) -> Result<u64, OutboxError> {
    let backend = runner_backend(&runner);
    let (sql, values) = build_delete_query(backend, scope);
    let stmt = Statement::from_sql_and_values(backend, &sql, values);
    let result = match &runner {
        SeaOrmRunner::Conn(c) => c.execute(stmt).await?,
        SeaOrmRunner::Tx(t) => t.execute(stmt).await?,
    };
    Ok(result.rows_affected())
}

struct QueryBuilder {
    sql: String,
    values: Vec<sea_orm::Value>,
    param_idx: usize,
    has_where: bool,
    is_mysql: bool,
    is_sqlite: bool,
}

impl QueryBuilder {
    fn new(base: &str, backend: DbBackend) -> Self {
        Self {
            sql: base.to_owned(),
            values: Vec::new(),
            param_idx: 1,
            has_where: false,
            is_mysql: backend == DbBackend::MySql,
            is_sqlite: backend == DbBackend::Sqlite,
        }
    }

    fn add_condition(&mut self, clause: &str, value: sea_orm::Value) {
        if self.has_where {
            self.sql.push_str(" AND ");
        } else {
            self.sql.push_str(" WHERE ");
            self.has_where = true;
        }
        if self.is_mysql {
            self.sql
                .push_str(&clause.replace(&format!("${}", self.param_idx), "?"));
        } else {
            self.sql.push_str(clause);
        }
        self.values.push(value);
        self.param_idx += 1;
    }

    fn add_raw_condition(&mut self, clause: &str) {
        if self.has_where {
            self.sql.push_str(" AND ");
        } else {
            self.sql.push_str(" WHERE ");
            self.has_where = true;
        }
        self.sql.push_str(clause);
    }

    fn finish(mut self, limit: Option<u32>) -> (String, Vec<sea_orm::Value>) {
        self.sql.push_str(" ORDER BY failed_at DESC");
        if let Some(n) = limit {
            #[allow(clippy::let_underscore_must_use)]
            let _ = write!(self.sql, " LIMIT {n}");
        }
        (self.sql, self.values)
    }

    fn finish_no_order(mut self, limit: Option<u32>) -> (String, Vec<sea_orm::Value>) {
        if let Some(n) = limit {
            #[allow(clippy::let_underscore_must_use)]
            let _ = write!(self.sql, " LIMIT {n}");
        }
        (self.sql, self.values)
    }

    fn finish_locking_no_order(
        self,
        limit: Option<u32>,
        for_update: bool,
    ) -> (String, Vec<sea_orm::Value>) {
        let is_sqlite = self.is_sqlite;
        let (mut sql, values) = self.finish_no_order(limit);
        if for_update && !is_sqlite {
            sql.push_str(" FOR UPDATE SKIP LOCKED");
        }
        (sql, values)
    }
}

fn apply_scope(qb: &mut QueryBuilder, scope: &DeadLetterScope) {
    if let Some(pid) = scope.partition_id {
        let idx = qb.param_idx;
        qb.add_condition(&format!("d.partition_id = ${idx}"), pid.into());
    }
    if let Some(ref queue) = scope.queue {
        let idx = qb.param_idx;
        qb.add_condition(
            &format!(
                "d.partition_id IN (SELECT id FROM modkit_outbox_partitions WHERE queue = ${idx})"
            ),
            queue.clone().into(),
        );
    }
    if let Some(ref payload_type) = scope.payload_type {
        let idx = qb.param_idx;
        qb.add_condition(
            &format!("d.payload_type = ${idx}"),
            payload_type.clone().into(),
        );
    }
}

fn apply_filter(qb: &mut QueryBuilder, filter: &DeadLetterFilter) {
    apply_scope(qb, &filter.scope);
    if let Some(status) = filter.status {
        let idx = qb.param_idx;
        qb.add_condition(&format!("d.status = ${idx}"), status.to_string().into());
    }
}

const SELECT_COLUMNS: &str = "SELECT d.id, d.partition_id, d.seq, d.payload, d.payload_type, \
     d.created_at, d.failed_at, d.last_error, d.attempts, d.status, d.completed_at, d.deadline \
     FROM modkit_outbox_dead_letters d";

fn build_select_query(
    backend: DbBackend,
    filter: &DeadLetterFilter,
) -> (String, Vec<sea_orm::Value>) {
    let mut qb = QueryBuilder::new(SELECT_COLUMNS, backend);
    apply_filter(&mut qb, filter);
    qb.finish(filter.scope.limit.or(Some(DEFAULT_DEAD_LETTER_LIMIT)))
}

fn build_count_query(
    backend: DbBackend,
    filter: &DeadLetterFilter,
) -> (String, Vec<sea_orm::Value>) {
    let mut qb = QueryBuilder::new(
        "SELECT COUNT(*) AS cnt FROM modkit_outbox_dead_letters d",
        backend,
    );
    apply_filter(&mut qb, filter);
    qb.finish_no_order(None)
}

/// Build a DELETE query for cleanup — only terminal states.
fn build_delete_query(
    backend: DbBackend,
    scope: &DeadLetterScope,
) -> (String, Vec<sea_orm::Value>) {
    let mut inner_qb = QueryBuilder::new("SELECT d.id FROM modkit_outbox_dead_letters d", backend);
    apply_scope(&mut inner_qb, scope);
    inner_qb.add_raw_condition("d.status IN ('resolved', 'discarded')");
    let (inner_sql, values) = inner_qb.finish_locking_no_order(scope.limit, true);
    let sql = format!("DELETE FROM modkit_outbox_dead_letters WHERE id IN ({inner_sql})");
    (sql, values)
}

/// Build the replay SELECT with orphan recovery: pending rows + expired reprocessing rows.
fn build_replay_select(
    backend: DbBackend,
    scope: &DeadLetterScope,
) -> (String, Vec<sea_orm::Value>) {
    let mut qb = QueryBuilder::new(SELECT_COLUMNS, backend);
    apply_scope(&mut qb, scope);
    let now_fn = db_now(backend);
    qb.add_raw_condition(&format!(
        "(d.status = 'pending' OR (d.status = 'reprocessing' AND d.deadline < {now_fn}))"
    ));
    // ORDER BY deadline NULLS FIRST (pending rows first), then id
    // Note: finish_no_order without limit — ORDER BY and LIMIT appended manually
    let is_sqlite = qb.is_sqlite;
    let (mut sql, values) = qb.finish_no_order(None);
    if is_sqlite {
        // SQLite: NULL sorts first by default with ASC
        sql.push_str(" ORDER BY d.deadline ASC, d.id ASC");
    } else {
        sql.push_str(" ORDER BY d.deadline ASC NULLS FIRST, d.id ASC");
    }
    if let Some(n) = scope.limit {
        #[allow(clippy::let_underscore_must_use)]
        let _ = write!(sql, " LIMIT {n}");
    }
    if !is_sqlite {
        sql.push_str(" FOR UPDATE SKIP LOCKED");
    }
    (sql, values)
}

/// Build `UPDATE ... SET status = 'reprocessing', deadline = now() + timeout WHERE id IN (...)`.
fn build_batch_claim(backend: DbBackend, count: usize) -> String {
    let is_mysql = backend == DbBackend::MySql;
    let now_fn = db_now(backend);
    let mut sql =
        String::from("UPDATE modkit_outbox_dead_letters SET status = 'reprocessing', deadline = ");
    match backend {
        DbBackend::Postgres => {
            #[allow(clippy::let_underscore_must_use)]
            let _ = write!(sql, "{now_fn} + $1 * INTERVAL '1 second'");
        }
        DbBackend::MySql => {
            #[allow(clippy::let_underscore_must_use)]
            let _ = write!(sql, "DATE_ADD({now_fn}, INTERVAL ? SECOND)");
        }
        DbBackend::Sqlite => {
            sql.push_str("datetime('now', '+' || $1 || ' seconds')");
        }
    }
    sql.push_str(" WHERE id IN (");
    for i in 0..count {
        if i > 0 {
            sql.push_str(", ");
        }
        if is_mysql {
            sql.push('?');
        } else {
            #[allow(clippy::let_underscore_must_use)]
            let _ = write!(sql, "${}", i + 2);
        }
    }
    sql.push(')');
    sql
}

/// Build `UPDATE ... SET status = 'resolved', completed_at = now(), deadline = NULL WHERE id IN (...) AND status = 'reprocessing'`.
fn build_batch_resolve(backend: DbBackend, count: usize) -> String {
    let is_mysql = backend == DbBackend::MySql;
    let now_fn = db_now(backend);
    let mut sql = format!(
        "UPDATE modkit_outbox_dead_letters SET status = 'resolved', completed_at = {now_fn}, deadline = NULL WHERE id IN ("
    );
    for i in 0..count {
        if i > 0 {
            sql.push_str(", ");
        }
        if is_mysql {
            sql.push('?');
        } else {
            #[allow(clippy::let_underscore_must_use)]
            let _ = write!(sql, "${}", i + 1);
        }
    }
    sql.push_str(") AND status = 'reprocessing'");
    sql
}

/// Build `UPDATE ... SET status = 'pending', attempts = attempts + 1, last_error = $reason, failed_at = now(), deadline = NULL WHERE id IN (...) AND status = 'reprocessing'`.
fn build_batch_reject(backend: DbBackend, count: usize) -> String {
    let is_mysql = backend == DbBackend::MySql;
    let now_fn = db_now(backend);
    // First param is the reason string, then IDs
    let mut sql = format!(
        "UPDATE modkit_outbox_dead_letters SET status = 'pending', attempts = attempts + 1, \
         last_error = {reason}, failed_at = {now_fn}, deadline = NULL WHERE id IN (",
        reason = if is_mysql { "?" } else { "$1" },
    );
    for i in 0..count {
        if i > 0 {
            sql.push_str(", ");
        }
        if is_mysql {
            sql.push('?');
        } else {
            #[allow(clippy::let_underscore_must_use)]
            let _ = write!(sql, "${}", i + 2);
        }
    }
    sql.push_str(") AND status = 'reprocessing'");
    sql
}

/// Build discard SELECT: pending rows only, with FOR UPDATE SKIP LOCKED.
fn build_discard_select(
    backend: DbBackend,
    scope: &DeadLetterScope,
) -> (String, Vec<sea_orm::Value>) {
    let mut qb = QueryBuilder::new("SELECT d.id FROM modkit_outbox_dead_letters d", backend);
    apply_scope(&mut qb, scope);
    qb.add_raw_condition("d.status = 'pending'");
    qb.finish_locking_no_order(scope.limit, true)
}

/// Build `UPDATE ... SET status = 'discarded', completed_at = now() WHERE id IN (...)`.
fn build_batch_discard(backend: DbBackend, count: usize) -> String {
    let is_mysql = backend == DbBackend::MySql;
    let now_fn = db_now(backend);
    let mut sql = format!(
        "UPDATE modkit_outbox_dead_letters SET status = 'discarded', completed_at = {now_fn} WHERE id IN ("
    );
    for i in 0..count {
        if i > 0 {
            sql.push_str(", ");
        }
        if is_mysql {
            sql.push('?');
        } else {
            #[allow(clippy::let_underscore_must_use)]
            let _ = write!(sql, "${}", i + 1);
        }
    }
    sql.push(')');
    sql
}

fn db_now(backend: DbBackend) -> &'static str {
    match backend {
        DbBackend::Postgres => "now()",
        DbBackend::MySql => "CURRENT_TIMESTAMP(6)",
        DbBackend::Sqlite => "datetime('now')",
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "dead_letter_tests.rs"]
mod tests;
