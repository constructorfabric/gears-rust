//! P-D-219 on `SQLite`: `m20260928_000009_unit_submit_note` adds the submitter's note,
//! `submit_note`, to `products_approval_unit`, through the approval library's separate step
//! (`bss_approval::ddl::apply_add_submit_note`; the library's `up()` is the body of `000003` and
//! stays as it shipped).
//!
//! Products has no schema golden, so this comparison is the proof, read by eye in the run's report.
//! A database is migrated by the gear's whole list without 000009 — the chain the deployment runs today —
//! then approval units are seeded with the values the application binds (a pending one and a
//! decided one, with an item and a decision), the structure of the table is captured, and the whole
//! list runs again through the real runner (`run_migrations_for_testing`), which applies 000009
//! alone. Exactly these facts may differ: the table's DDL (the column appended) and the new
//! `table_info` row. Every unit survives, reads null for the note in SQL and through the gear's own
//! repository, an upgraded database equals a fresh one, and a replay applies nothing. The Postgres
//! twin is `postgres_unit_submit_note_migration.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_products::gear::BssProductsGear;
use bss_products::infra::storage::repo;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

const MIGRATION: &str = "m20260928_000009_unit_submit_note";
const TABLE: &str = "products_approval_unit";
const TENANT: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0009_0001);
const PENDING: Uuid = Uuid::from_u128(0x0009_0001);
const DECIDED: Uuid = Uuid::from_u128(0x0009_0002);
const SKU: Uuid = Uuid::from_u128(0x0009_0051);

/// A file database in its own temporary directory, so the runner's pool, the repository and a raw
/// connection see one schema, and the directory goes with the test.
struct Lite {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl Lite {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("products-unit-note-")
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

    /// The gear's whole migration list through the toolkit runner, on its own pool; `without`
    /// leaves one migration out, as the deployed database's chain does before this run.
    async fn migrate(&self, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
        let chain = BssProductsGear::default()
            .migrations()
            .into_iter()
            .filter(|m| Some(m.name()) != without)
            .collect();
        run_migrations_for_testing(&self.pool().await, chain).await
    }

    async fn raw(&self) -> DatabaseConnection {
        Database::connect(self.dsn()).await.unwrap()
    }

    /// One text column `v` per row.
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

    /// The structure of the table, one fact per line, sorted.
    async fn structure(&self) -> Vec<String> {
        let mut facts = self
            .strings(&format!(
                "SELECT type || ' ' || name || ': ' || coalesce(sql, '(auto)') AS v \
                 FROM sqlite_master WHERE tbl_name = '{TABLE}'"
            ))
            .await;
        facts.extend(
            self.strings(&format!(
                "SELECT 'table_info: ' || cid || ' ' || name || ' ' || type || ' notnull=' || \
                 \"notnull\" || ' dflt=' || coalesce(dflt_value, '(none)') || ' pk=' || pk AS v \
                 FROM pragma_table_info('{TABLE}')"
            ))
            .await,
        );
        facts.sort();
        facts
    }

    async fn columns(&self) -> Vec<String> {
        self.strings(&format!(
            "SELECT name AS v FROM pragma_table_info('{TABLE}') ORDER BY cid"
        ))
        .await
    }

    /// Every unit row as JSON of `columns` (blobs as hex), by id.
    async fn rows(&self, columns: &[String]) -> Vec<String> {
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
            "SELECT json_object({pairs}) AS v FROM {TABLE} ORDER BY id"
        ))
        .await
    }
}

/// The deployed database before this run: the chain without 000009, and two units written with the
/// values the application binds — one pending, one rejected with its decision note — each with its
/// item, and the rejection's decision row.
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
        (DECIDED, "rejected", Some(at), Some("not now")),
    ] {
        raw.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO products_approval_unit (id, tenant_id, kind, ref_type, ref_id, state, \
             common_effective_date, quorum_required, generation, submitted_by, submitted_at, \
             decided_at, decided_note, snapshot, snapshot_hash, version) \
             VALUES (?, ?, 'sku_change', 'sku', ?, ?, NULL, 1, 1, ?, ?, ?, ?, ?, 'h', 2)",
            [
                id.into(),
                TENANT.into(),
                SKU.into(),
                state.into(),
                Uuid::from_u128(0xa0).into(),
                at.into(),
                decided_at.into(),
                decided_note.into(),
                serde_json::json!({"name": "Renamed"}).into(),
            ],
        ))
        .await
        .unwrap();
        raw.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO products_approval_unit_item (unit_id, tenant_id, item_type, item_id, \
             created_by, before_json, after_json) VALUES (?, ?, 'sku', ?, ?, NULL, ?)",
            [
                id.into(),
                TENANT.into(),
                SKU.into(),
                Uuid::from_u128(0xa0).into(),
                serde_json::json!({"name": "Renamed"}).into(),
            ],
        ))
        .await
        .unwrap();
    }
    raw.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO products_approval_decision (unit_id, tenant_id, actor, generation, decision, \
         note, at, stale) VALUES (?, ?, ?, 1, 'reject', 'not now', ?, 0)",
        [
            DECIDED.into(),
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
async fn the_note_arrives_empty_and_every_unit_survives() {
    let db = seeded().await;
    let columns_before = db.columns().await;
    let structure_before = db.structure().await;
    let rows_before = db.rows(&columns_before).await;
    assert_eq!(rows_before.len(), 2, "{rows_before:#?}");

    let result = db.migrate(None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000009 was pending");
    let structure_after = db.structure().await;
    let removed: Vec<&String> = structure_before
        .iter()
        .filter(|f| !structure_after.contains(f))
        .collect();
    let added: Vec<&String> = structure_after
        .iter()
        .filter(|f| !structure_before.contains(f))
        .collect();
    let show = |facts: &[&String]| {
        facts
            .iter()
            .map(|f| f.as_str())
            .collect::<Vec<_>>()
            .join("\n  ")
    };
    eprintln!(
        "P-D-219 SQLite proof: {} structural facts before, {} after, {} rows\nremoved:\n  {}\nadded:\n  {}",
        structure_before.len(),
        structure_after.len(),
        rows_before.len(),
        show(&removed),
        show(&added)
    );
    assert_eq!(removed.len(), 1, "removed: {removed:#?}\nadded: {added:#?}");
    assert_eq!(added.len(), 2, "removed: {removed:#?}\nadded: {added:#?}");
    // The table: `ADD COLUMN` appends the column definition and changes nothing else.
    let table_before = removed[0];
    assert!(
        table_before.starts_with("table products_approval_unit: "),
        "{table_before}"
    );
    let table_after = added
        .iter()
        .find(|f| f.starts_with("table products_approval_unit: "))
        .unwrap();
    assert_eq!(
        table_after.as_str(),
        format!(
            "{}, submit_note text)",
            table_before.strip_suffix(')').unwrap()
        )
    );
    let n = columns_before.len();
    assert!(
        added.contains(&&format!(
            "table_info: {n} submit_note TEXT notnull=0 dflt=(none) pk=0"
        )),
        "{added:#?}"
    );
    assert_eq!(db.columns().await[n..], ["submit_note"]);

    // Every unit survives, and a unit written before the migration reads null.
    assert_eq!(db.rows(&columns_before).await, rows_before);
    assert_eq!(
        db.strings(&format!(
            "SELECT coalesce(submit_note, 'null') AS v FROM {TABLE} ORDER BY id"
        ))
        .await,
        ["null", "null"]
    );

    // An upgraded database and a fresh one hold the same table.
    let fresh = Lite::new();
    fresh.migrate(None).await.unwrap();
    assert_eq!(fresh.structure().await, structure_after);
    // A replay applies nothing.
    assert!(db.migrate(None).await.unwrap().applied_names.is_empty());
}

/// The gear's own repository reads a unit written before the migration: its note is null, and
/// everything else it stored reads as it was.
#[tokio::test]
async fn the_gear_reads_a_unit_written_before_the_migration_with_no_note() {
    let db = seeded().await;
    db.migrate(None).await.unwrap();
    let provider = DBProvider::<toolkit_db::DbError>::new(db.pool().await);
    let conn = provider.conn().unwrap();
    let scope = AccessScope::for_tenant(TENANT);
    let units = repo::list_units(&conn, &scope, TENANT, None, None, Some(SKU))
        .await
        .unwrap();
    assert_eq!(units.len(), 2);
    for unit in &units {
        assert_eq!(unit.submit_note, None, "{unit:?}");
        assert_eq!(unit.kind, "sku_change");
        assert_eq!(unit.version, 2);
    }
    let decided = repo::find_unit(&conn, &scope, TENANT, DECIDED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(decided.decided_note.as_deref(), Some("not now"));
    assert_eq!(decided.submit_note, None);
    let decisions = repo::decisions_of(&conn, &scope, TENANT, DECIDED)
        .await
        .unwrap();
    assert_eq!(decisions.len(), 1);
}
