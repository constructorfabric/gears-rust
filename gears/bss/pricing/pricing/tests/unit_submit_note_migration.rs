//! D-445 on `SQLite`: the forward migration `m20260928_000016_unit_submit_note` adds the
//! submitter's note, `submit_note`, to `pricing_approval_unit`, through the approval library's
//! separate step (`bss_approval::ddl::apply_add_submit_note`; the library's `up()` is the body of
//! `000002` and stays as it shipped).
//!
//! The deployed database is the chain without 000016, with approval units written by the values the
//! application binds (a pending prices unit and a rejected one, their items, the rejection). The
//! whole list then runs through the real runner and applies 000016 alone: the schema dump differs
//! by the one column, the table's text by the appended definition, every unit survives and reads a
//! null note — in SQL and through the gear's repository — an upgraded database equals a fresh one,
//! and a replay applies nothing. The Postgres twin is `postgres_unit_submit_note_migration.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod schema_dump;

use bss_pricing::infra::storage::repo::approval_repo;
use bss_pricing::module::BssPricingGear;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use serde_json::Value;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

const MIGRATION: &str = "m20260928_000016_unit_submit_note";
const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0016_0001);
const BOOK: Uuid = Uuid::from_u128(0x0016_0b00);
const PENDING: Uuid = Uuid::from_u128(0x0016_0001);
const REJECTED: Uuid = Uuid::from_u128(0x0016_0002);

/// A file database in its own temporary directory, so the runner's pool, the repository and a raw
/// connection see one schema, and the directory goes with the test.
struct Lite {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}
impl Lite {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("pricing-unit-note-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("db.sqlite3");
        Self { _dir: dir, path }
    }
    fn dsn(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
    async fn pool(&self) -> toolkit_db::Db {
        connect_db(
            &self.dsn(),
            ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..ConnectOpts::default()
            },
        )
        .await
        .unwrap()
    }
    async fn migrate(&self, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
        let chain = BssPricingGear::default()
            .migrations()
            .into_iter()
            .filter(|m| Some(m.name()) != without)
            .collect();
        run_migrations_for_testing(&self.pool().await, chain).await
    }
    async fn raw(&self) -> DatabaseConnection {
        Database::connect(self.dsn()).await.unwrap()
    }
    async fn strings(&self, sql: &str) -> Vec<String> {
        let raw = self.raw().await;
        let rows = raw
            .query_all_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .iter()
            .map(|row| row.try_get::<String>("", "v").unwrap())
            .collect();
        raw.close().await.unwrap();
        rows
    }
    async fn dump(&self) -> Vec<String> {
        let raw = self.raw().await;
        let dump = schema_dump::sqlite_dump(&raw).await;
        raw.close().await.unwrap();
        stanza_lines(&dump)
    }
    async fn ddl(&self) -> String {
        self.strings(
            "SELECT sql AS v FROM sqlite_master WHERE type = 'table' \
             AND name = 'pricing_approval_unit'",
        )
        .await
        .pop()
        .unwrap()
    }
    /// Every unit row: every column, blobs as hex, as JSON, by id.
    async fn rows(&self) -> Vec<Value> {
        let columns = self
            .strings(
                "SELECT name AS v FROM pragma_table_info('pricing_approval_unit') ORDER BY cid",
            )
            .await;
        let pairs = columns
            .iter()
            .map(|c| {
                format!(
                    "'{c}', CASE typeof(\"{c}\") WHEN 'blob' THEN 'x:' || hex(\"{c}\") ELSE \"{c}\" END"
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        self.strings(&format!(
            "SELECT json_object({pairs}) AS v FROM pricing_approval_unit ORDER BY id"
        ))
        .await
        .iter()
        .map(|r| serde_json::from_str(r).unwrap())
        .collect()
    }
}
/// Each dump line prefixed with the stanza it belongs to; the runner's ledger left out.
fn stanza_lines(dump: &str) -> Vec<String> {
    let mut stanza = String::new();
    let mut lines = Vec::new();
    for line in dump.lines() {
        if !line.starts_with(' ') {
            line.clone_into(&mut stanza);
        }
        if stanza.contains("toolkit_migrations") {
            continue;
        }
        if line.starts_with(' ') {
            lines.push(format!("{stanza} ::{line}"));
        } else {
            lines.push(line.to_owned());
        }
    }
    lines
}
/// The deployed database before this run: the chain without 000016, a pending prices unit and a
/// rejected one with their items and the rejection, bound as the application binds them.
async fn seeded() -> Lite {
    let db = Lite::new();
    let before = db.migrate(Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    let at = time::OffsetDateTime::parse(
        "2026-09-27T09:00:00.123456Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let raw = db.raw().await;
    for (id, state, decided_at, decided_note) in [
        (PENDING, "pending", None, None),
        (REJECTED, "rejected", Some(at), Some("too cheap")),
    ] {
        raw.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO pricing_approval_unit (id, tenant_id, kind, ref_type, ref_id, state, \
             common_effective_date, quorum_required, generation, submitted_by, submitted_at, \
             decided_at, decided_note, snapshot, snapshot_hash, version) \
             VALUES (?, ?, 'prices', 'price_book', ?, ?, NULL, 1, 1, ?, ?, ?, ?, ?, 'h', 2)",
            [
                id.into(),
                TENANT.into(),
                BOOK.into(),
                state.into(),
                Uuid::from_u128(0xa0).into(),
                at.into(),
                decided_at.into(),
                decided_note.into(),
                serde_json::json!({"prices": 1}).into(),
            ],
        ))
        .await
        .unwrap();
        raw.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO pricing_approval_unit_item (unit_id, tenant_id, item_type, item_id, \
             created_by, before_json, after_json) VALUES (?, ?, 'price', ?, ?, NULL, ?)",
            [
                id.into(),
                TENANT.into(),
                Uuid::new_v4().into(),
                Uuid::from_u128(0xa0).into(),
                serde_json::json!({"amount": "10"}).into(),
            ],
        ))
        .await
        .unwrap();
    }
    raw.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO pricing_approval_decision (unit_id, tenant_id, actor, generation, decision, \
         note, at, stale) VALUES (?, ?, ?, 1, 'reject', 'too cheap', ?, 0)",
        [
            REJECTED.into(),
            TENANT.into(),
            Uuid::from_u128(0xb0).into(),
            at.into(),
        ],
    ))
    .await
    .unwrap();
    raw.close().await.unwrap();
    db
}

#[tokio::test]
async fn the_forward_migration_adds_the_note_and_keeps_every_unit() {
    let db = seeded().await;
    let dump_before = db.dump().await;
    let ddl_before = db.ddl().await;
    let rows_before = db.rows().await;
    assert_eq!(rows_before.len(), 2);

    let result = db.migrate(None).await.unwrap();
    assert_eq!(result.applied_names, [MIGRATION], "only 000016 was pending");

    let dump_after = db.dump().await;
    let removed: Vec<&String> = dump_before
        .iter()
        .filter(|l| !dump_after.contains(l))
        .collect();
    let added: Vec<&String> = dump_after
        .iter()
        .filter(|l| !dump_before.contains(l))
        .collect();
    let show = |lines: &[&String]| {
        lines
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    };
    eprintln!(
        "000016 SQLite dump diff\nremoved:\n  {}\nadded:\n  {}",
        show(&removed),
        show(&added)
    );
    assert!(removed.is_empty(), "{removed:#?}");
    assert_eq!(
        added,
        ["TABLE pricing_approval_unit ::  COLUMN submit_note TEXT NULL DEFAULT - PK 0"]
    );
    let ddl_after = db.ddl().await;
    eprintln!("000016 SQLite table text after:\n{ddl_after}");
    assert_eq!(
        ddl_after.replace(", submit_note text", ""),
        ddl_before,
        "ADD COLUMN appends the column and nothing else"
    );
    // Every unit survives: every old column as it was, the note NULL.
    let rows_after: Vec<Value> = db
        .rows()
        .await
        .into_iter()
        .map(|mut row| {
            assert_eq!(
                row.as_object_mut().unwrap().remove("submit_note"),
                Some(Value::Null)
            );
            row
        })
        .collect();
    assert_eq!(rows_after, rows_before);
    // An upgraded database and a fresh one hold the same schema; a replay applies nothing.
    let fresh = Database::connect("sqlite::memory:").await.unwrap();
    assert_eq!(
        dump_after,
        stanza_lines(&schema_dump::migrate_and_dump_sqlite(&fresh).await)
    );
    assert!(db.migrate(None).await.unwrap().applied_names.is_empty());
}

/// The gear's repository reads a unit written before the migration: no note, the rest as stored.
#[tokio::test]
async fn the_gear_reads_a_unit_written_before_the_migration_with_no_note() {
    let db = seeded().await;
    db.migrate(None).await.unwrap();
    let provider = DBProvider::<toolkit_db::DbError>::new(db.pool().await);
    let conn = provider.conn().unwrap();
    let scope = AccessScope::for_tenant(TENANT);
    let units = approval_repo::list_units(&conn, &scope, TENANT, None, None, Some(BOOK))
        .await
        .unwrap();
    assert_eq!(units.len(), 2);
    assert!(units.iter().all(|u| u.submit_note.is_none()), "{units:?}");
    let rejected = approval_repo::find_unit(&conn, &scope, TENANT, REJECTED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rejected.decided_note.as_deref(), Some("too cheap"));
    assert_eq!(rejected.version, 2);
    assert_eq!(
        approval_repo::decisions_of(&conn, &scope, TENANT, REJECTED)
            .await
            .unwrap()
            .len(),
        1
    );
}
