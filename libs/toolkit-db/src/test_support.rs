// Created: 2026-07-27 by Constructor Tech
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::doc_markdown)]
//! Test-only support for DB-behavior audits: a SQL query recorder.
//!
//! Gated behind the `test-support` feature; never compiled into production
//! builds. Nothing here is gear-specific — any crate that wants to audit its
//! own database behavior enables the feature as a dev-dependency and uses
//! these types directly, rather than copying them.
//!
//! [`QueryRecorder`] attaches a `SeaORM` metric callback to a connection
//! *before* it is wrapped into a [`crate::DBProvider`] (see
//! [`connect_with_recorder`]), so every statement the service layer issues is
//! captured: normalized SQL text (literals redacted, variadic placeholder
//! lists collapsed so batch size doesn't change the "shape"), statement kind,
//! a best-effort target table, bound-parameter count, and whether it executed
//! while the transaction-bypass guard was armed (i.e. inside a
//! `Db::transaction*` closure).
//!
//! # Why not observe literal BEGIN/COMMIT/ROLLBACK?
//!
//! `SeaORM`'s SQLite and Postgres drivers issue transaction-boundary SQL
//! through a lower-level path — `sqlx`'s `TransactionManager`, which for
//! SQLite talks to the connection's dedicated worker thread directly — that
//! bypasses the `Statement`/metric-callback machinery entirely. There is no
//! `Info` event for `BEGIN`/`COMMIT`/`ROLLBACK`. Transaction membership is
//! therefore inferred from the transaction-bypass guard, exposed as
//! [`crate::secure::in_transaction_for_testing`]. That guard is armed for
//! exactly the scope of a `Db::transaction*` closure — the same task-local the
//! production bypass guard enforces — and since a metric callback fires
//! synchronously on the same async task that issued the query (no
//! `tokio::spawn` in between), reading it from inside the callback is exact,
//! not a heuristic. Its one structural blind spot is a detached
//! `tokio::spawn`, whose task does not inherit the task-local.
//!
//! The worked example this was extracted from, including what the method does
//! and does not cover, is `gears/system/resource-group/docs/db-behavior-audit.md`;
//! the method itself is `docs/toolkit_unified_system/14_db_behavior_testing.md`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
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
    /// Monotonic sequence number, in execution order.
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
    /// Number of bound parameter values in this statement (e.g. an `IN (?,
    /// ?, ?)` list of 3 contributes 3). Statement *count* is scale-invariant
    /// for a well-batched query (one `IN (...)` regardless of N), but the
    /// parameter count still grows with N -- this exists so scale-invariance
    /// checks can budget for that separately from statement count. See the
    /// audit report's "what this method does not cover" section: this
    /// doesn't capture the cost of a single huge statement (e.g. a 10,000-
    /// value `IN` list), only that it has 10,000 parameters.
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
/// [`QueryRecorder::untransacted_read_modify_write`]) return the statements they
/// flagged precisely so a failing assertion can show them — otherwise whoever
/// reads the failure has to re-run with `DB_AUDIT_TRACE_DIR` set before they can
/// judge anything.
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
// Postgres numbers its placeholders (`$1`, `$2`, ...) where SQLite/MySQL use a
// bare `?`. Rewrite them to `?` so the same logical statement normalizes
// identically on either backend — and so the list collapse below needs only one
// pattern. This has to run *before* `RE_NUMBER`, which would otherwise turn
// `$1` into `$?` and leave nothing for this to match.
static RE_PG_PLACEHOLDER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\d+").expect("valid regex"));
// Collapse a run of 2+ `?` placeholders into a single marker, so an `IN (...)`
// list or a multi-row `INSERT ... VALUES` batch normalizes the same way
// regardless of N.
static RE_PLACEHOLDER_LIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\?(?:\s*,\s*\?)+").expect("valid regex"));

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
/// This is deliberately a *class*-level signature, not per-line matching: it
/// groups statements by "what shape of query is this", which is what the
/// scale-invariance and stats-by-`(kind, table)` rules need. It is a
/// best-effort heuristic (regex over text, not a SQL parser) — documented as
/// such in `docs/db-behavior-audit.md`.
#[must_use]
pub fn normalize_sql(sql: &str) -> String {
    let s = RE_STRING_LIT.replace_all(sql, "'?'");
    // Order matters: Postgres placeholders before numeric literals, and the
    // list collapse last, once every placeholder is a bare `?`.
    let s = RE_PG_PLACEHOLDER.replace_all(&s, "?");
    let s = RE_NUMBER.replace_all(&s, "?");
    let s = RE_PLACEHOLDER_LIST.replace_all(&s, "?");
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
    /// aggregation methods (`stats`, `writes_outside_tx`,
    /// `redundant_reads_after_write`, `dump`) can be unit-tested without a
    /// live DB connection.
    #[cfg(test)]
    fn from_events_for_testing(events: Vec<RecordedQuery>) -> Self {
        Self {
            events: Arc::new(Mutex::new(events)),
        }
    }

    /// Build a fresh recorder and the `SeaORM` metric callback that feeds it.
    ///
    /// Callers normally don't build this directly — use
    /// [`connect_with_recorder`], which attaches the callback to a
    /// connection *before* wrapping it in a `DBProvider` (required: `SeaORM`
    /// captures the callback by value at query time, so attaching after
    /// construction would miss every statement).
    #[must_use = "the recorder observes nothing unless its callback is passed to \
                  connect_db_with_metric_callback before the connection is wrapped"]
    pub fn attach() -> (
        Self,
        impl Fn(&sea_orm::metric::Info<'_>) + Send + Sync + 'static,
    ) {
        let events: Arc<Mutex<Vec<RecordedQuery>>> = Arc::new(Mutex::new(Vec::new()));
        let seq = Arc::new(AtomicUsize::new(0));
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
            let param_count = info
                .statement
                .values
                .as_ref()
                .map_or(0, |values| values.0.len());
            let n = seq.fetch_add(1, Ordering::Relaxed);
            let rec = RecordedQuery {
                seq: n,
                kind,
                table,
                sql,
                raw_sql: info.statement.to_string(),
                in_tx,
                param_count,
                elapsed: info.elapsed,
                failed: info.failed,
            };
            events
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(rec);
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

    /// Sum of `param_count` across every captured statement. A companion
    /// budget to `total()`/`stats()`: a batched query (single `IN (...)`
    /// regardless of N) keeps statement *count* flat as N grows, but its
    /// parameter count still scales with N -- this catches that dimension.
    #[must_use]
    pub fn total_params(&self) -> usize {
        self.events().iter().map(|e| e.param_count).sum()
    }

    /// Clear the recorded trace. Each audit test normally builds a fresh
    /// database instead of reusing a recorder, but this is handy for the
    /// recorder's own unit tests.
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
    /// was *not* armed — i.e. on a bare connection, outside any
    /// `Db::transaction*` closure.
    ///
    /// **This is a primitive, not a verdict.** A lone self-contained write
    /// outside a transaction is perfectly fine: PostgreSQL already runs every
    /// statement in its own implicit transaction, so wrapping one statement in
    /// `BEGIN`/`COMMIT` buys no atomicity and costs two extra round trips (three
    /// under `SERIALIZABLE`, which also has to set the isolation level). Do not
    /// assert this is empty across the board — you would be demanding a
    /// pessimization.
    ///
    /// Use it when you know a *particular* operation must be transactional and
    /// want that pinned. For finding the operations that *should* be
    /// transactional and aren't, use [`Self::untransacted_write_runs`] and
    /// [`Self::untransacted_read_modify_write`], which narrow this set to the
    /// two shapes that are actually suspicious.
    #[must_use]
    pub fn writes_outside_tx(&self) -> Vec<RecordedQuery> {
        self.events()
            .into_iter()
            .filter(|e| is_write(e.kind))
            .filter(|e| !e.in_tx)
            .collect()
    }

    /// Runs of two or more writes that executed outside a transaction with no
    /// transaction boundary between them — several mutations that nothing binds
    /// into one atomic unit, so a crash or an error part-way through leaves the
    /// earlier ones committed.
    ///
    /// **Advisory: this marks a place to read by eye, not a proven defect.**
    /// Independent writes that genuinely need no mutual atomicity (appending to
    /// a log, bumping unrelated counters) land here too. The question to ask of
    /// each run is whether a caller could observe the intermediate state, and
    /// whether that state is legal.
    ///
    /// Reads between the writes do **not** split a run: a `SELECT` doesn't make
    /// the writes around it atomic. What splits a run is a statement that *did*
    /// run inside a transaction, since the writes on either side of it are then
    /// separated by a real boundary.
    ///
    /// Each returned `Vec` is one run, in execution order, so it can be printed
    /// with [`format_trace`] in the assertion message.
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

    /// Writes that executed outside a transaction against a table this trace
    /// had already *read* outside a transaction — the check-then-act (or
    /// read-modify-write) shape, where a concurrent caller can interleave
    /// between the read and the write and invalidate what the read established.
    ///
    /// This is the signal [`Self::untransacted_write_runs`] cannot give: the
    /// classic case is many reads and a *single* write, which is not a run of
    /// anything.
    ///
    /// **Advisory, same as above.** It fires on benign races too — re-deleting
    /// a row that a competitor already deleted reaches the same end state, so
    /// the interleaving is harmless there. What makes a hit real is an invariant
    /// that the read established and the write relies on.
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
    /// followed — before any other statement touches `T` again — by a
    /// `SELECT` on the same table `T`. Matches the "insert/update, discard
    /// the returned model, re-read by id" pattern.
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
            writeln!(
                out,
                "{:>3}  {:<7} {:<32} in_tx={:<5} params={:<3} {}",
                e.seq,
                e.kind,
                e.table.as_deref().unwrap_or("-"),
                e.in_tx,
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
/// **Opt-in and module-agnostic.** Writes nothing unless `DB_AUDIT_TRACE_DIR`
/// is set, so an ordinary test run never touches the filesystem, and the
/// destination comes entirely from that variable — nothing here knows or cares
/// which crate it is running in:
///
/// ```sh
/// DB_AUDIT_TRACE_DIR=target/db-behavior-traces \
///     cargo nextest run -p <gear> --test db_behavior_audit_test
/// ```
///
/// Note the absence of `--run-ignored`: any `#[ignore]`d assertion in the
/// suite is one that asserts behavior the code does *not* yet have, so forcing
/// those to run fails the command before the trace tests get their turn.
///
/// Reading these dumps once, before writing a single assertion, is the highest
/// yield step of a DB-behavior audit: most findings are visible as soon as an
/// operation's statement sequence is laid out in order. The resource-group
/// audit that produced this tooling is the worked example — see "Running this
/// audit on another module" in
/// `gears/system/resource-group/docs/db-behavior-audit.md` — and the method is
/// `docs/toolkit_unified_system/14_db_behavior_testing.md`.
///
/// `name` becomes `<DB_AUDIT_TRACE_DIR>/<name>.txt`; use the operation's name.
///
/// # Panics
///
/// Panics if `DB_AUDIT_TRACE_DIR` is set but the directory cannot be created
/// or the file cannot be written. That is deliberate: the variable is only ever
/// set by someone who asked for a trace dump, so silently producing nothing
/// would be worse than failing the test run.
pub fn snapshot_trace(name: &str, rec: &QueryRecorder) {
    let Some(dir) = std::env::var_os("DB_AUDIT_TRACE_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let path = dir.join(format!("{name}.txt"));
    std::fs::write(&path, rec.dump()).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// Connect and return a [`Db`] with a [`QueryRecorder`] already attached.
///
/// The callback has to be installed before the connection is wrapped, which is
/// why this pairs the two steps. Migrations and any `DBProvider` wrapping stay
/// with the caller, since those are schema-specific; a typical fixture is:
///
/// ```ignore
/// let (db, recorder) = connect_with_recorder("sqlite::memory:", opts).await?;
/// run_migrations_for_testing(&db, Migrator::migrations()).await?;
/// recorder.clear(); // migrations ran through the same callback
/// (Arc::new(DBProvider::new(db)), recorder)
/// ```
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

// -- Unit tests for the recorder's own classification helpers --
//
// `from_sql`/`extract_table` are private to this module, so they're tested
// here rather than from an integration test (which can only see `pub`
// items). `normalize_sql`'s table-driven cases and the aggregation
// methods (`stats`, `writes_outside_tx`, `redundant_reads_after_write`,
// `dump`) are public, and each consuming crate additionally proves the live
// wiring end to end.
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
            (
                r#"INSERT INTO "resource_group_closure" VALUES ($1, $2), ($3, $4)"#,
                r#"INSERT INTO "resource_group_closure" VALUES (?), (?)"#,
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
