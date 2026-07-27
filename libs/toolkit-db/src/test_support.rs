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

// -- SQL normalization -------------------------------------------------

static RE_STRING_LIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"'(?:[^']|'')*'").expect("valid regex"));
static RE_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d+\b").expect("valid regex"));
static RE_WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("valid regex"));
// Collapse a run of 2+ `?` placeholders (SQLite/MySQL style) or `$1, $2, ...`
// (Postgres style) into a single canonical marker, so an `IN (...)` list or a
// multi-row `INSERT ... VALUES` batch normalizes the same way regardless of N.
static RE_PLACEHOLDER_LIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\?(?:\s*,\s*\?)+").expect("valid regex"));
static RE_PG_PLACEHOLDER_LIST: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\d+(?:\s*,\s*\$\d+)+").expect("valid regex"));

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
    let s = RE_NUMBER.replace_all(&s, "?");
    let s = RE_PLACEHOLDER_LIST.replace_all(&s, "?");
    let s = RE_PG_PLACEHOLDER_LIST.replace_all(&s, "$1");
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

    /// Writes (`INSERT`/`UPDATE`/`DELETE`) that ran while the transaction
    /// guard was *not* armed — i.e. issued on a bare connection outside any
    /// `Db::transaction*` closure. A non-empty result is the `no-tx-write`
    /// defect class: a check-then-write sequence with no atomicity guarantee.
    #[must_use]
    pub fn writes_outside_tx(&self) -> Vec<RecordedQuery> {
        self.events()
            .into_iter()
            .filter(|e| {
                matches!(
                    e.kind,
                    QueryKind::Insert | QueryKind::Update | QueryKind::Delete
                )
            })
            .filter(|e| !e.in_tx)
            .collect()
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
        ];
        for (input, expected) in cases {
            assert_eq!(super::normalize_sql(input), expected, "input: {input}");
        }
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
