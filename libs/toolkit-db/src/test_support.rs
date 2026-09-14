// Created: 2026-07-27 by Constructor Tech
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! Test-only support for DB-behavior audits: a SQL query recorder.
//!
//! Gated behind the `test-support` feature; never compiled into production
//! builds. Not gear-specific: any crate enables the feature as a
//! dev-dependency and uses these types directly.
//!
//! [`crate::test_support::QueryRecorder`] attaches a `SeaORM` metric callback
//! to a connection *before* it is wrapped into a [`crate::DBProvider`] (see
//! [`crate::test_support::connect_with_recorder`]) so every statement is
//! captured.
//!
//! Captured per statement: normalized SQL (literals redacted, placeholder
//! lists collapsed so batch size doesn't change the "shape"), kind, target
//! table, parameter count, whether the transaction-bypass guard was armed,
//! and which transaction (if any) it ran inside.
//!
//! # Why not observe literal BEGIN/COMMIT/ROLLBACK?
//!
//! `SeaORM`'s drivers issue transaction-boundary SQL through `sqlx`'s
//! `TransactionManager`, bypassing the metric-callback machinery -- there is
//! no `Info` event for `BEGIN`/`COMMIT`/`ROLLBACK`.
//!
//! Transaction membership is instead read from the transaction-bypass guard
//! ([`crate::secure::in_transaction_for_testing`]), the same task-local the
//! production guard enforces. That answers "in a transaction or not", but not
//! "in the *same* transaction as this other statement" -- two statements can
//! each be `in_tx: true` while running in two different, sequential
//! transactions. [`crate::secure::transaction_id_for_testing`] answers that:
//! a fresh id every time a transaction begins, so [`RecordedQuery::tx_id`]
//! lets [`QueryRecorder::all_in_one_transaction`] tell the two cases apart.
//!
//! The callback fires synchronously on the task that issued the query, so
//! reading the guard is exact, not a heuristic; its only blind spot is a
//! detached `tokio::spawn`, whose task doesn't inherit the task-local.
//!
//! Full method and worked example:
//! `gears/system/resource-group/docs/db-behavior-audit.md`,
//! `docs/toolkit_unified_system/14_db_behavior_testing.md`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use regex::Regex;

/// Coarse SQL statement kind, derived from the leading keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum QueryKind {
    Select,
    Insert,
    Update,
    Delete,
    /// `PRAGMA`, migration DDL, and anything else we don't classify.
    Other,
}

impl QueryKind {
    fn from_sql(sql: &str) -> Self {
        match sql
            .split_whitespace()
            .next()
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            Some("SELECT") => Self::Select,
            Some("INSERT") => Self::Insert,
            Some("UPDATE") => Self::Update,
            Some("DELETE") => Self::Delete,
            _ => Self::Other,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Select => "SELECT",
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
            Self::Delete => "DELETE",
            Self::Other => "OTHER",
        }
    }
}

impl std::fmt::Display for QueryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.pad(self.as_str())
    }
}

/// One captured statement.
#[derive(Debug, Clone)]
pub struct RecordedQuery {
    /// Position of this statement in the trace, in execution order.
    ///
    /// Assigned under the same lock that appends the statement, so it is
    /// always the index into [`QueryRecorder::events`] -- including after a
    /// [`QueryRecorder::clear`], which starts a new trace at 0. Use it to
    /// order statements against each other, not as an identifier that
    /// outlives the trace.
    pub seq: usize,
    pub kind: QueryKind,
    /// Best-effort target table, extracted from the raw SQL text.
    pub table: Option<String>,
    /// Normalized SQL: literals redacted, placeholder lists collapsed.
    pub sql: String,
    /// Human-readable SQL with bound values injected back in (via `SeaORM`'s
    /// own `Statement` `Display` impl) -- for trace dumps, not for matching.
    pub raw_sql: String,
    /// Whether this statement executed while the transaction-bypass guard
    /// was armed (i.e. inside a `Db::transaction*` closure).
    pub in_tx: bool,
    /// Identity of the transaction this statement ran inside, or `None`
    /// outside any transaction. Two statements can both have `in_tx: true`
    /// while running in two different (e.g. sequential) transactions --
    /// `tx_id` is what lets a test tell that case apart from "genuinely the
    /// same transaction". See [`crate::secure::transaction_id_for_testing`].
    pub tx_id: Option<u64>,
    /// Isolation level the transaction this statement ran inside was *asked*
    /// to open at, or `None` when it was opened at the backend default (or
    /// when the statement ran outside a transaction -- `in_tx` tells those
    /// two apart).
    ///
    /// Records the request rather than what the engine did with it, which is
    /// what makes it useful on `SQLite`: `SQLite` is serializable regardless,
    /// so an invariant that several paths must all open `SERIALIZABLE` --
    /// so `PostgreSQL`'s SSI can see them as conflicting -- leaves no trace in
    /// the statements, the counts or the results. Downgrading one path is
    /// invisible to every other kind of assertion in this recorder.
    /// See [`crate::secure::transaction_isolation_for_testing`].
    pub tx_isolation: Option<crate::secure::TxIsolationLevel>,
    /// Number of bound parameters (an `IN (?, ?, ?)` list of 3 contributes
    /// 3). Statement count stays flat for a batched query as N grows, but
    /// parameter count doesn't -- this lets scale checks budget separately.
    pub param_count: usize,
    pub elapsed: Duration,
    pub failed: bool,
}

fn is_write(kind: QueryKind) -> bool {
    matches!(
        kind,
        QueryKind::Insert | QueryKind::Update | QueryKind::Delete
    )
}

/// Render captured statements one per line, for an assertion message.
///
/// The advisory signals ([`QueryRecorder::untransacted_write_runs`],
/// [`QueryRecorder::untransacted_read_modify_write`]) return the flagged
/// statements precisely so a failing assertion can show them directly.
#[must_use]
pub fn format_trace(queries: &[RecordedQuery]) -> String {
    let mut out = String::new();
    for q in queries {
        writeln!(
            out,
            "{:>3}  {:<7} {:<32} in_tx={:<5} params={:<3} {}",
            q.seq,
            q.kind,
            q.table.as_deref().unwrap_or("-"),
            q.in_tx,
            q.param_count,
            q.sql,
        )
        .expect("String Write is infallible");
    }
    out
}

// -- SQL normalization -------------------------------------------------

static RE_STRING_LIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"'(?:[^']|'')*'").expect("valid regex"));
static RE_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d+\b").expect("valid regex"));
static RE_WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid regex"));
// Postgres numbers placeholders (`$1`, `$2`, ...); rewrite to bare `?` so
// both backends normalize identically and the list-collapse regex below
// needs only one pattern. Must run before RE_NUMBER, or `$1` becomes `$?`.
static RE_PG_PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\d+").expect("valid regex"));
// Collapse a run of 2+ `?` placeholders into a single marker, so an `IN (...)`
// list normalizes the same way regardless of N.
static RE_PLACEHOLDER_LIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\?(?:\s*,\s*\?)+").expect("valid regex"));
// Collapse a run of 2+ parenthesized placeholder groups into one. The regex
// above only reaches inside a single group: the `), (` separator of a
// multi-row `INSERT ... VALUES` stops it, so without this a two-row batch and
// a three-row batch would carry different signatures and a scale rule
// grouping by the normalized text would compare nothing. Runs after it, once
// every group has been reduced to `(?)`.
static RE_VALUES_GROUP_LIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\(\?\)(?:\s*,\s*\(\?\))+").expect("valid regex"));

static RE_INSERT_TABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^insert\s+into\s+"?([a-zA-Z_][a-zA-Z0-9_]*)"?"#).expect("valid regex")
});
static RE_UPDATE_TABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^update\s+"?([a-zA-Z_][a-zA-Z0-9_]*)"?"#).expect("valid regex")
});
static RE_DELETE_TABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^delete\s+from\s+"?([a-zA-Z_][a-zA-Z0-9_]*)"?"#).expect("valid regex")
});
static RE_FROM_TABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bfrom\s+"?([a-zA-Z_][a-zA-Z0-9_]*)"?"#).expect("valid regex")
});

/// Normalize raw SQL text into a scale/literal-independent signature.
///
/// A class-level signature grouping statements by "what shape of query is
/// this", which scale-invariance and stats-by-`(kind, table)` rules need.
/// A best-effort heuristic (regex, not a SQL parser).
#[must_use]
pub fn normalize_sql(sql: &str) -> String {
    let s = RE_STRING_LIT.replace_all(sql, "'?'");
    // Order matters: Postgres placeholders before numeric literals, and the
    // list collapse last, once every placeholder is a bare `?`.
    let s = RE_PG_PLACEHOLDER.replace_all(&s, "?");
    let s = RE_NUMBER.replace_all(&s, "?");
    let s = RE_PLACEHOLDER_LIST.replace_all(&s, "?");
    let s = RE_VALUES_GROUP_LIST.replace_all(&s, "(?)");
    let s = RE_WHITESPACE.replace_all(&s, " ");
    s.trim().to_owned()
}

fn extract_table(kind: QueryKind, raw_sql: &str) -> Option<String> {
    let re = match kind {
        QueryKind::Insert => &*RE_INSERT_TABLE,
        QueryKind::Update => &*RE_UPDATE_TABLE,
        QueryKind::Delete => &*RE_DELETE_TABLE,
        QueryKind::Select => &*RE_FROM_TABLE,
        QueryKind::Other => return None,
    };
    re.captures(raw_sql).map(|c| c[1].to_owned())
}

// -- Recorder ------------------------------------------------------------

/// Shared handle to a captured SQL trace. Cheap to clone (shares the
/// underlying event log).
#[derive(Clone)]
pub struct QueryRecorder {
    events: Arc<Mutex<Vec<RecordedQuery>>>,
}

impl QueryRecorder {
    /// Test-only: build a recorder pre-loaded with a fixed trace, so the
    /// aggregation methods can be unit-tested without a live DB connection.
    #[cfg(test)]
    fn from_events_for_testing(events: Vec<RecordedQuery>) -> Self {
        Self {
            events: Arc::new(Mutex::new(events)),
        }
    }

    /// Build a fresh recorder and the `SeaORM` metric callback that feeds it.
    ///
    /// Callers normally use [`connect_with_recorder`] instead: `SeaORM`
    /// captures the callback by value at query time, so it must attach
    /// before the connection is wrapped in a `DBProvider`, not after.
    #[must_use = "the recorder observes nothing unless its callback is passed to \
                  connect_db_with_metric_callback before the connection is wrapped"]
    pub fn attach() -> (
        Self,
        impl Fn(&sea_orm::metric::Info<'_>) + Send + Sync + 'static,
    ) {
        let events: Arc<Mutex<Vec<RecordedQuery>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = Self {
            events: Arc::clone(&events),
        };

        let callback = move |info: &sea_orm::metric::Info<'_>| {
            let raw_sql = info.statement.sql.clone();
            let kind = QueryKind::from_sql(&raw_sql);
            let table = extract_table(kind, &raw_sql);
            let sql = normalize_sql(&raw_sql);
            // Precise, not a heuristic -- see module docs.
            let in_tx = crate::secure::in_transaction_for_testing();
            let tx_id = crate::secure::transaction_id_for_testing();
            let tx_isolation = crate::secure::transaction_isolation_for_testing().flatten();
            let param_count = info
                .statement
                .values
                .as_ref()
                .map_or(0, |values| values.0.len());
            // The number is taken under the same lock that appends, so it is
            // the row's own index by construction. A separate counter cannot
            // give that: it has to be incremented before the lock is taken,
            // so two statements racing here could be appended in the opposite
            // order to the one they were numbered in -- and `clear()` would
            // then need a reset that races them again.
            let mut events = events.lock().unwrap_or_else(PoisonError::into_inner);
            let seq = events.len();
            events.push(RecordedQuery {
                seq,
                kind,
                table,
                sql,
                raw_sql: info.statement.to_string(),
                in_tx,
                tx_id,
                tx_isolation,
                param_count,
                elapsed: info.elapsed,
                failed: info.failed,
            });
        };

        (recorder, callback)
    }

    /// All captured statements, in execution order.
    #[must_use]
    pub fn events(&self) -> Vec<RecordedQuery> {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[must_use]
    pub fn total(&self) -> usize {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Sum of `param_count` across every statement -- companion to
    /// `total()`/`stats()`, since a batched query keeps statement count flat
    /// as N grows even though its parameter count still scales with N.
    #[must_use]
    pub fn total_params(&self) -> usize {
        self.events().iter().map(|e| e.param_count).sum()
    }

    /// Clear the recorded trace. Each audit test normally builds a fresh
    /// database instead of reusing a recorder, but this is handy for the
    /// recorder's own unit tests.
    ///
    /// Numbering restarts with the trace: `seq` is the row's index in
    /// [`Self::events`], so the first statement recorded after a clear is
    /// again `0`. Nothing separate has to be reset for that to hold.
    pub fn clear(&self) {
        self.events
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Counts grouped by `(kind, table)`. Table is `"<none>"` when no table
    /// could be extracted (e.g. `PRAGMA`, migration DDL).
    #[must_use]
    pub fn stats(&self) -> BTreeMap<(QueryKind, String), usize> {
        let mut out: BTreeMap<(QueryKind, String), usize> = BTreeMap::new();
        for e in self.events() {
            let table = e.table.unwrap_or_else(|| "<none>".to_owned());
            *out.entry((e.kind, table)).or_insert(0) += 1;
        }
        out
    }

    /// Every `INSERT`/`UPDATE`/`DELETE` that ran while the transaction guard
    /// was not armed -- i.e. on a bare connection, outside any
    /// `Db::transaction*` closure.
    ///
    /// **Not a verdict on its own.** A lone self-contained write is fine:
    /// PostgreSQL already runs it in an implicit transaction, so wrapping it
    /// in `BEGIN`/`COMMIT` buys nothing. Don't assert this is empty overall.
    ///
    /// Use it to pin a *particular* operation as transactional. To find
    /// operations that *should* be transactional and aren't, use
    /// [`Self::untransacted_write_runs`] and [`Self::untransacted_read_modify_write`].
    #[must_use]
    pub fn writes_outside_tx(&self) -> Vec<RecordedQuery> {
        self.events()
            .into_iter()
            .filter(|e| is_write(e.kind))
            .filter(|e| !e.in_tx)
            .collect()
    }

    /// Whether every `in_tx` event in the trace ran inside one and the same
    /// transaction.
    ///
    /// Stronger than "every event was `in_tx`": that check passes just as
    /// well if a regression split one transactional operation into two
    /// sequential transactions, since each event is still "in a transaction"
    /// on its own. This compares [`RecordedQuery::tx_id`] across the `in_tx`
    /// events instead, so it catches that split.
    ///
    /// Deliberately silent on out-of-tx events -- pairing this with
    /// [`Self::writes_outside_tx`] covers that, and plenty of correct traces
    /// have a stateless pre-validation read before `BEGIN` that this must not
    /// fail on. `false` if there are no `in_tx` events at all, or if they
    /// span more than one transaction id.
    #[must_use]
    pub fn all_in_one_transaction(&self) -> bool {
        let mut in_tx_ids = self
            .events()
            .into_iter()
            .filter(|e| e.in_tx)
            .map(|e| e.tx_id);
        let Some(first_id) = in_tx_ids.next() else {
            return false;
        };
        first_id.is_some() && in_tx_ids.all(|id| id == first_id)
    }

    /// Whether every `in_tx` event in the trace ran inside a transaction that
    /// was opened at `SERIALIZABLE`.
    ///
    /// For invariants that hold only when *several* code paths all open that
    /// level -- so `PostgreSQL`'s SSI tracks them as conflicting with each
    /// other -- no other check in this recorder can see a regression. Lowering
    /// one path's level changes no statement, no count, no ordering, and no
    /// result on `SQLite`, which is serializable regardless. This reads the
    /// level the path *asked* for, which is the thing under review.
    ///
    /// Like [`Self::all_in_one_transaction`], silent on out-of-tx events and
    /// `false` when there are none in a transaction at all.
    #[must_use]
    pub fn all_in_serializable_transaction(&self) -> bool {
        let mut levels = self
            .events()
            .into_iter()
            .filter(|e| e.in_tx)
            .map(|e| e.tx_isolation)
            .peekable();
        levels.peek().is_some()
            && levels.all(|level| level == Some(crate::secure::TxIsolationLevel::Serializable))
    }

    /// Runs of two or more untransacted writes with no transaction boundary
    /// between them -- mutations nothing binds into one atomic unit, so a
    /// crash part-way through leaves the earlier ones committed.
    ///
    /// **Advisory, not a proven defect.** Independent writes needing no
    /// mutual atomicity (log appends, unrelated counters) land here too; ask
    /// whether a caller could observe the intermediate state, and if it's legal.
    ///
    /// A `SELECT` between writes does not split a run; only a statement that
    /// ran inside a transaction does, since that's a real boundary.
    ///
    /// Each `Vec` is one run, in order -- pass it to [`format_trace`] for the
    /// assertion message.
    #[must_use]
    pub fn untransacted_write_runs(&self) -> Vec<Vec<RecordedQuery>> {
        let mut runs = Vec::new();
        let mut current: Vec<RecordedQuery> = Vec::new();
        for e in self.events() {
            if e.in_tx {
                // A real transaction boundary: whatever follows is no longer
                // adjacent to what came before.
                if current.len() >= 2 {
                    runs.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            } else if is_write(e.kind) {
                current.push(e);
            }
        }
        if current.len() >= 2 {
            runs.push(current);
        }
        runs
    }

    /// Writes outside a transaction against a table this trace already read
    /// outside a transaction -- the check-then-act shape, where a concurrent
    /// caller can interleave and invalidate what the read established.
    ///
    /// Catches what [`Self::untransacted_write_runs`] cannot: many reads and
    /// a single write, which is not a run of anything.
    ///
    /// **Advisory, same as above.** Fires on benign races too (e.g.
    /// re-deleting an already-deleted row); a hit is real only when the
    /// write relies on an invariant the read established.
    #[must_use]
    pub fn untransacted_read_modify_write(&self) -> Vec<RecordedQuery> {
        let mut read_tables: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for e in self.events() {
            let Some(table) = e.table.clone() else {
                continue;
            };
            if e.in_tx {
                continue;
            }
            if e.kind == QueryKind::Select {
                read_tables.insert(table);
            } else if is_write(e.kind) && read_tables.contains(&table) {
                out.push(e);
            }
        }
        out
    }

    /// Best-effort `redundant-io` detector: an `INSERT`/`UPDATE` on table `T`
    /// immediately followed by a `SELECT` on `T` -- the "insert, discard the
    /// model, re-read by id" pattern.
    #[must_use]
    pub fn redundant_reads_after_write(&self) -> Vec<(RecordedQuery, RecordedQuery)> {
        let events = self.events();
        let mut out = Vec::new();
        for w in events
            .iter()
            .filter(|e| matches!(e.kind, QueryKind::Insert | QueryKind::Update))
        {
            let Some(table) = w.table.as_deref() else {
                continue;
            };
            let next_same_table = events
                .iter()
                .find(|e| e.seq > w.seq && e.table.as_deref() == Some(table));
            if let Some(next) = next_same_table
                && next.kind == QueryKind::Select
            {
                out.push((w.clone(), next.clone()));
            }
        }
        out
    }

    /// Human-readable trace, one line per statement, with synthetic markers
    /// at transaction-scope transitions (see module docs for why these are
    /// synthetic rather than literal `BEGIN`/`COMMIT` statements).
    #[must_use]
    pub fn dump(&self) -> String {
        let mut out = String::new();
        let mut last_in_tx = false;
        for (i, e) in self.events().into_iter().enumerate() {
            if i == 0 || e.in_tx != last_in_tx {
                let marker = if e.in_tx {
                    "-- [enter tx scope] --"
                } else {
                    "-- [outside tx] --"
                };
                writeln!(out, "{marker}").expect("String Write is infallible");
            }
            last_in_tx = e.in_tx;
            let tx_id = e.tx_id.map_or_else(|| "-".to_owned(), |id| id.to_string());
            let iso = match e.tx_isolation {
                Some(level) => format!("{level:?}"),
                None => "-".to_owned(),
            };
            writeln!(
                out,
                "{:>3}  {:<7} {:<32} in_tx={:<5} tx={:<4} iso={:<15} params={:<3} {}",
                e.seq,
                e.kind,
                e.table.as_deref().unwrap_or("-"),
                e.in_tx,
                tx_id,
                iso,
                e.param_count,
                e.sql,
            )
            .expect("String Write is infallible");
        }
        out
    }
}

/// Dump a recorded trace to a file, for reading a write path's SQL by eye.
///
/// Writes nothing unless `DB_AUDIT_TRACE_DIR` is set, so an ordinary test
/// run never touches the filesystem:
///
/// ```sh
/// DB_AUDIT_TRACE_DIR=target/db-behavior-traces cargo nextest run -p <gear> --test db_behavior_audit_test
/// ```
///
/// Omit `--run-ignored`: a `#[ignore]`d assertion documents behavior the
/// code does not yet have, so forcing it to run fails before the trace
/// tests get their turn.
///
/// `name` becomes `<DB_AUDIT_TRACE_DIR>/<name>.txt`. Reading a dump once,
/// before writing an assertion, is usually the highest-yield step of an
/// audit; see `gears/system/resource-group/docs/db-behavior-audit.md`.
///
/// # Panics
///
/// Panics if `DB_AUDIT_TRACE_DIR` is set but the file can't be written --
/// silently producing nothing would be worse than failing the test run.
pub fn snapshot_trace(name: &str, rec: &QueryRecorder) {
    let Some(dir) = std::env::var_os("DB_AUDIT_TRACE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let path = dir.join(format!("{name}.txt"));
    std::fs::write(&path, rec.dump()).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Connect and return a [`crate::Db`] with a [`QueryRecorder`] already
/// attached.
///
/// The callback must be installed before the connection is wrapped, hence
/// this pairs the two steps. Migrations run through the same callback, so
/// `clear()` the recorder after running them and before handing it to callers.
///
/// # Errors
///
/// Returns [`DbError`](crate::DbError) under the same conditions as
/// [`crate::connect_db`].
pub async fn connect_with_recorder(
    dsn: &str,
    opts: crate::ConnectOpts,
) -> crate::Result<(crate::Db, QueryRecorder)> {
    let (recorder, callback) = QueryRecorder::attach();
    let db = crate::connect_db_with_metric_callback(dsn, opts, callback).await?;
    Ok((db, recorder))
}

// Unit tests for the recorder's own classification helpers.
//
// `from_sql`/`extract_table` are private, so they're tested here rather
// than from an integration test, which can only see `pub` items.
#[cfg(test)]
mod tests {
    use super::{QueryKind, extract_table};

    #[test]
    fn kind_from_sql_classifies_by_leading_keyword() {
        let cases: Vec<(&str, QueryKind)> = vec![
            ("SELECT * FROM foo", QueryKind::Select),
            ("  select id from foo", QueryKind::Select),
            ("INSERT INTO foo (a) VALUES (?)", QueryKind::Insert),
            ("UPDATE foo SET a = ?", QueryKind::Update),
            ("DELETE FROM foo WHERE id = ?", QueryKind::Delete),
            ("PRAGMA foreign_keys = ON", QueryKind::Other),
            ("CREATE TABLE foo (id INTEGER)", QueryKind::Other),
            ("BEGIN", QueryKind::Other),
        ];
        for (sql, expected) in cases {
            assert_eq!(QueryKind::from_sql(sql), expected, "sql: {sql}");
        }
    }

    #[test]
    fn extract_table_finds_target_per_kind() {
        let cases: Vec<(QueryKind, &str, Option<&str>)> = vec![
            (
                QueryKind::Insert,
                r#"INSERT INTO "resource_group" ("id") VALUES (?)"#,
                Some("resource_group"),
            ),
            (
                QueryKind::Update,
                r#"UPDATE "gts_type" SET "name" = ? WHERE "id" = ?"#,
                Some("gts_type"),
            ),
            (
                QueryKind::Delete,
                r#"DELETE FROM "resource_group_closure" WHERE "descendant_id" = ?"#,
                Some("resource_group_closure"),
            ),
            (
                QueryKind::Select,
                r#"SELECT "id" FROM "resource_group" WHERE "id" = ?"#,
                Some("resource_group"),
            ),
            (QueryKind::Other, "PRAGMA foreign_keys = ON", None),
        ];
        for (kind, sql, expected) in cases {
            let actual = extract_table(kind, sql);
            assert_eq!(actual.as_deref(), expected, "sql: {sql}");
        }
    }

    #[test]
    fn normalize_sql_table_driven_cases() {
        let cases: Vec<(&str, &str)> = vec![
            (
                "SELECT  *   FROM foo\nWHERE id = 42",
                "SELECT * FROM foo WHERE id = ?",
            ),
            (
                "SELECT * FROM foo WHERE name = 'literal value, with comma'",
                "SELECT * FROM foo WHERE name = '?'",
            ),
            (
                r#"SELECT * FROM "gts_type" WHERE "id" IN (?, ?, ?)"#,
                r#"SELECT * FROM "gts_type" WHERE "id" IN (?)"#,
            ),
            // Postgres numbers its placeholders; a single one normalizes to the
            // same bare `?` SQLite produces.
            (
                r#"SELECT * FROM "gts_type" WHERE "id" = $1"#,
                r#"SELECT * FROM "gts_type" WHERE "id" = ?"#,
            ),
            // ...and a Postgres list collapses like a SQLite one, rather than
            // being deleted or left with per-index numbering.
            (
                r#"SELECT * FROM "gts_type" WHERE "id" IN ($1, $2, $3)"#,
                r#"SELECT * FROM "gts_type" WHERE "id" IN (?)"#,
            ),
            // A multi-row batch collapses to one group, so its signature does
            // not encode the batch size...
            (
                r#"INSERT INTO "resource_group_closure" VALUES ($1, $2), ($3, $4)"#,
                r#"INSERT INTO "resource_group_closure" VALUES (?)"#,
            ),
            // ...and a wider batch of the same statement lands on that same
            // signature rather than growing a group per row.
            (
                r#"INSERT INTO "resource_group_closure" VALUES ($1, $2), ($3, $4), ($5, $6)"#,
                r#"INSERT INTO "resource_group_closure" VALUES (?)"#,
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(super::normalize_sql(input), expected, "input: {input}");
        }
    }

    #[test]
    fn normalize_sql_pg_and_sqlite_placeholders_agree() {
        // The same logical statement issued against either backend must land on
        // one signature, or per-(kind, table) stats would split in two on
        // Postgres and the scale-invariance rules would compare nothing.
        assert_eq!(
            super::normalize_sql(r#"SELECT * FROM "gts_type" WHERE "id" IN ($1, $2, $3)"#),
            super::normalize_sql(r#"SELECT * FROM "gts_type" WHERE "id" IN (?, ?, ?)"#),
        );
    }

    #[test]
    fn normalize_sql_pg_placeholder_list_length_does_not_change_shape() {
        let three = super::normalize_sql(r#"SELECT * FROM "gts_type" WHERE "id" IN ($1, $2, $3)"#);
        let thirty = super::normalize_sql(&format!(
            r#"SELECT * FROM "gts_type" WHERE "id" IN ({})"#,
            (1..=30)
                .map(|i| format!("${i}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        assert_eq!(
            three, thirty,
            "Postgres IN-list length must not change the normalized shape"
        );
    }

    #[test]
    fn normalize_sql_placeholder_list_length_does_not_change_shape() {
        let three = super::normalize_sql(r#"SELECT * FROM "gts_type" WHERE "id" IN (?, ?, ?)"#);
        let thirty = super::normalize_sql(&format!(
            r#"SELECT * FROM "gts_type" WHERE "id" IN ({})"#,
            vec!["?"; 30].join(", ")
        ));
        assert_eq!(
            three, thirty,
            "IN-list length must not change the normalized shape (scale-invariance grouping)"
        );
    }

    fn make(seq: usize, kind: QueryKind, table: Option<&str>, in_tx: bool) -> super::RecordedQuery {
        super::RecordedQuery {
            seq,
            kind,
            table: table.map(str::to_owned),
            sql: format!("<stmt {seq}>"),
            raw_sql: format!("<stmt {seq}>"),
            in_tx,
            tx_id: None,
            tx_isolation: None,
            param_count: 0,
            elapsed: std::time::Duration::ZERO,
            failed: false,
        }
    }

    #[test]
    fn total_params_sums_param_count_across_events() {
        let mut a = make(0, QueryKind::Select, Some("gts_type"), false);
        a.param_count = 3;
        let mut b = make(1, QueryKind::Insert, Some("resource_group"), true);
        b.param_count = 5;
        let rec = super::QueryRecorder::from_events_for_testing(vec![a, b]);
        assert_eq!(rec.total_params(), 8);
    }

    #[test]
    fn stats_groups_by_kind_and_table() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Select, Some("gts_type"), false),
            make(1, QueryKind::Select, Some("gts_type"), false),
            make(2, QueryKind::Insert, Some("resource_group"), true),
            make(3, QueryKind::Other, None, false),
        ]);
        let stats = rec.stats();
        assert_eq!(
            stats.get(&(QueryKind::Select, "gts_type".to_owned())),
            Some(&2)
        );
        assert_eq!(
            stats.get(&(QueryKind::Insert, "resource_group".to_owned())),
            Some(&1)
        );
        assert_eq!(
            stats.get(&(QueryKind::Other, "<none>".to_owned())),
            Some(&1)
        );
        assert_eq!(rec.total(), 4);
    }

    // -- untransacted_write_runs: >=2 adjacent writes with no tx between --

    #[test]
    fn write_runs_flags_two_adjacent_untransacted_writes() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Insert, Some("resource_group"), false),
            make(1, QueryKind::Insert, Some("resource_group_closure"), false),
        ]);
        let runs = rec.untransacted_write_runs();
        assert_eq!(
            runs.len(),
            1,
            "two adjacent untransacted writes are one run"
        );
        assert_eq!(runs[0].len(), 2);
    }

    #[test]
    fn write_runs_ignores_a_lone_untransacted_write() {
        // The whole point of this signal over `writes_outside_tx`: one
        // self-contained write outside a transaction is already atomic in
        // PostgreSQL and must not be reported.
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Select, Some("gts_type"), false),
            make(1, QueryKind::Insert, Some("audit_log"), false),
        ]);
        assert!(
            rec.untransacted_write_runs().is_empty(),
            "a single untransacted write is not a run and is not a defect"
        );
    }

    #[test]
    fn write_runs_are_not_split_by_reads() {
        // A SELECT between two writes does not make them atomic, so it must not
        // break the run.
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Insert, Some("resource_group"), false),
            make(1, QueryKind::Select, Some("gts_type"), false),
            make(2, QueryKind::Delete, Some("resource_group_closure"), false),
        ]);
        let runs = rec.untransacted_write_runs();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].len(), 2, "the read is not part of the run itself");
    }

    #[test]
    fn write_runs_are_split_by_a_transaction() {
        // Writes on either side of a transacted statement are separated by a
        // real boundary, so neither side reaches two.
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Insert, Some("resource_group"), false),
            make(1, QueryKind::Insert, Some("resource_group"), true),
            make(2, QueryKind::Insert, Some("resource_group"), false),
        ]);
        assert!(rec.untransacted_write_runs().is_empty());
    }

    #[test]
    fn write_runs_ignores_transacted_writes() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Insert, Some("resource_group"), true),
            make(1, QueryKind::Insert, Some("resource_group_closure"), true),
            make(2, QueryKind::Delete, Some("resource_group_closure"), true),
        ]);
        assert!(rec.untransacted_write_runs().is_empty());
    }

    // -- untransacted_read_modify_write: check-then-act --

    #[test]
    fn read_modify_write_flags_write_after_read_of_the_same_table() {
        // Many reads and a *single* write: the shape `untransacted_write_runs`
        // cannot see, and the one that broke a real invariant.
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Select, Some("resource_group"), false),
            make(
                1,
                QueryKind::Select,
                Some("resource_group_membership"),
                false,
            ),
            make(
                2,
                QueryKind::Insert,
                Some("resource_group_membership"),
                false,
            ),
        ]);
        let flagged = rec.untransacted_read_modify_write();
        assert_eq!(flagged.len(), 1, "check-then-act on the membership table");
        assert_eq!(flagged[0].seq, 2);
    }

    #[test]
    fn read_modify_write_ignores_an_unrelated_table() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Select, Some("gts_type"), false),
            make(1, QueryKind::Insert, Some("audit_log"), false),
        ]);
        assert!(rec.untransacted_read_modify_write().is_empty());
    }

    #[test]
    fn read_modify_write_requires_the_read_to_come_first() {
        // Insert then re-read is the `redundant-io` shape, not check-then-act.
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Insert, Some("resource_group"), false),
            make(1, QueryKind::Select, Some("resource_group"), false),
        ]);
        assert!(rec.untransacted_read_modify_write().is_empty());
    }

    #[test]
    fn read_modify_write_ignores_the_same_shape_inside_a_transaction() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(
                0,
                QueryKind::Select,
                Some("resource_group_membership"),
                true,
            ),
            make(
                1,
                QueryKind::Insert,
                Some("resource_group_membership"),
                true,
            ),
        ]);
        assert!(rec.untransacted_read_modify_write().is_empty());
    }

    #[test]
    fn format_trace_renders_one_line_per_statement() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Insert, Some("resource_group"), false),
            make(1, QueryKind::Delete, Some("resource_group"), false),
        ]);
        let rendered = super::format_trace(&rec.untransacted_write_runs()[0]);
        assert_eq!(rendered.lines().count(), 2);
        assert!(rendered.contains("resource_group"));
    }

    #[test]
    fn writes_outside_tx_flags_only_untransacted_writes() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Select, Some("gts_type"), false), // read, ignored regardless of in_tx
            make(
                1,
                QueryKind::Insert,
                Some("resource_group_membership"),
                false,
            ), // flagged
            make(2, QueryKind::Insert, Some("resource_group"), true), // in tx, clean
            make(3, QueryKind::Delete, Some("resource_group"), false), // flagged
        ]);
        let flagged = rec.writes_outside_tx();
        assert_eq!(
            flagged.len(),
            2,
            "expected exactly the two untransacted writes"
        );
        assert!(flagged.iter().all(|e| !e.in_tx));
        assert_eq!(flagged[0].seq, 1);
        assert_eq!(flagged[1].seq, 3);
    }

    #[test]
    fn writes_outside_tx_empty_when_all_writes_are_transacted() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Select, Some("gts_type"), false),
            make(1, QueryKind::Insert, Some("gts_type"), true),
            make(2, QueryKind::Update, Some("gts_type"), true),
        ]);
        assert!(rec.writes_outside_tx().is_empty());
    }

    #[test]
    fn all_in_one_transaction_true_when_every_in_tx_event_shares_one_tx_id() {
        let mut a = make(0, QueryKind::Select, Some("gts_type"), true);
        a.tx_id = Some(7);
        let mut b = make(1, QueryKind::Insert, Some("gts_type"), true);
        b.tx_id = Some(7);
        let rec = super::QueryRecorder::from_events_for_testing(vec![a, b]);
        assert!(rec.all_in_one_transaction());
    }

    #[test]
    fn all_in_one_transaction_false_across_two_sequential_transactions() {
        // Both events are `in_tx: true`, so `writes_outside_tx` alone would
        // not catch this: a regression that ran the read and the write as
        // two separate, sequential transactions instead of one.
        let mut a = make(0, QueryKind::Select, Some("gts_type"), true);
        a.tx_id = Some(7);
        let mut b = make(1, QueryKind::Insert, Some("gts_type"), true);
        b.tx_id = Some(8);
        let rec = super::QueryRecorder::from_events_for_testing(vec![a, b]);
        assert!(!rec.all_in_one_transaction());
    }

    #[test]
    fn all_in_one_transaction_true_despite_a_pre_transaction_read() {
        // A stateless read before `BEGIN` is a legitimate, common shape in
        // this codebase (pre-validation outside the transaction) -- it must
        // not fail this check on its own. `writes_outside_tx` is what would
        // catch an actual *write* escaping the transaction; this function is
        // silent on out-of-tx events entirely, by design.
        let pre_read = make(0, QueryKind::Select, Some("gts_type"), false);
        let mut a = make(1, QueryKind::Select, Some("gts_type"), true);
        a.tx_id = Some(7);
        let mut b = make(2, QueryKind::Insert, Some("gts_type"), true);
        b.tx_id = Some(7);
        let rec = super::QueryRecorder::from_events_for_testing(vec![pre_read, a, b]);
        assert!(rec.all_in_one_transaction());
    }

    #[test]
    fn all_in_one_transaction_false_when_no_event_is_in_a_transaction() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![make(
            0,
            QueryKind::Select,
            Some("gts_type"),
            false,
        )]);
        assert!(!rec.all_in_one_transaction());
    }

    #[test]
    fn all_in_one_transaction_false_for_an_empty_trace() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![]);
        assert!(!rec.all_in_one_transaction());
    }

    #[test]
    fn all_in_serializable_transaction_true_when_every_in_tx_event_is_serializable() {
        let mut a = make(0, QueryKind::Select, Some("resource_group"), true);
        a.tx_isolation = Some(crate::secure::TxIsolationLevel::Serializable);
        let mut b = make(
            1,
            QueryKind::Insert,
            Some("resource_group_membership"),
            true,
        );
        b.tx_isolation = Some(crate::secure::TxIsolationLevel::Serializable);
        let rec = super::QueryRecorder::from_events_for_testing(vec![a, b]);
        assert!(rec.all_in_serializable_transaction());
    }

    #[test]
    fn all_in_serializable_transaction_false_at_the_backend_default() {
        // `None` is what `TxConfig::default()` records: opened, but at
        // whatever the backend's own level is.
        let mut a = make(
            0,
            QueryKind::Delete,
            Some("resource_group_membership"),
            true,
        );
        a.tx_isolation = None;
        let rec = super::QueryRecorder::from_events_for_testing(vec![a]);
        assert!(!rec.all_in_serializable_transaction());
    }

    #[test]
    fn all_in_serializable_transaction_false_when_one_event_is_weaker() {
        // The case the helper exists for: a mixed trace, where one path was
        // lowered and the rest were not.
        let mut a = make(0, QueryKind::Select, Some("gts_type"), true);
        a.tx_isolation = Some(crate::secure::TxIsolationLevel::Serializable);
        let mut b = make(1, QueryKind::Insert, Some("gts_type"), true);
        b.tx_isolation = Some(crate::secure::TxIsolationLevel::ReadCommitted);
        let rec = super::QueryRecorder::from_events_for_testing(vec![a, b]);
        assert!(!rec.all_in_serializable_transaction());
    }

    #[test]
    fn all_in_serializable_transaction_ignores_a_pre_transaction_read() {
        // Same shape `all_in_one_transaction` tolerates: a stateless read
        // before `BEGIN` must not fail the check.
        let before = make(0, QueryKind::Select, Some("gts_type"), false);
        let mut inside = make(1, QueryKind::Insert, Some("gts_type"), true);
        inside.tx_isolation = Some(crate::secure::TxIsolationLevel::Serializable);
        let rec = super::QueryRecorder::from_events_for_testing(vec![before, inside]);
        assert!(rec.all_in_serializable_transaction());
    }

    #[test]
    fn all_in_serializable_transaction_false_when_no_event_is_in_a_transaction() {
        let a = make(0, QueryKind::Select, Some("gts_type"), false);
        let rec = super::QueryRecorder::from_events_for_testing(vec![a]);
        assert!(!rec.all_in_serializable_transaction());
    }

    #[test]
    fn all_in_serializable_transaction_false_for_an_empty_trace() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![]);
        assert!(!rec.all_in_serializable_transaction());
    }

    #[test]
    fn redundant_reads_after_write_flags_reread_of_same_table() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Insert, Some("resource_group"), true),
            make(1, QueryKind::Select, Some("resource_group"), true), // redundant re-read
            make(2, QueryKind::Insert, Some("resource_group_closure"), true),
            make(3, QueryKind::Insert, Some("resource_group_closure"), true), // another write first, not a read
        ]);
        let hits = rec.redundant_reads_after_write();
        assert_eq!(
            hits.len(),
            1,
            "only the insert-then-select-same-table pair should match"
        );
        assert_eq!(hits[0].0.seq, 0);
        assert_eq!(hits[0].1.seq, 1);
    }

    #[test]
    fn redundant_reads_after_write_empty_when_no_reread_follows() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Insert, Some("resource_group"), true),
            make(1, QueryKind::Select, Some("gts_type"), true), // different table -- not a re-read
        ]);
        assert!(rec.redundant_reads_after_write().is_empty());
    }

    #[test]
    fn dump_marks_tx_scope_transitions() {
        let rec = super::QueryRecorder::from_events_for_testing(vec![
            make(0, QueryKind::Select, Some("gts_type"), false),
            make(1, QueryKind::Insert, Some("resource_group"), true),
            make(2, QueryKind::Insert, Some("resource_group_closure"), true),
        ]);
        let dump = rec.dump();
        assert_eq!(
            dump.matches("[outside tx]").count(),
            1,
            "one transition into the outside-tx region:\n{dump}"
        );
        assert_eq!(
            dump.matches("[enter tx scope]").count(),
            1,
            "one transition into the tx region:\n{dump}"
        );
        assert!(dump.contains("resource_group_closure"));
    }
}
