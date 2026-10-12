//! P-D-220 on `SQLite`: `m20260928_000010_clear_retired_defaults` clears every retired default the
//! deployed database stored before the doors refused one, so "a retired category is never the default" holds
//! for stored rows too.
//!
//! Products has no schema golden: the migration changes data only, and this test shows the
//! table's structure is the same before and after it. A database is migrated by the gear's whole
//! list without 000010 — the chain the deployment runs today — then categories are seeded with the
//! values the application binds: a tenant whose default was retired before P-D-220 (and an
//! active category of it), and a tenant with an active default (and a retired category that is
//! not the default). The whole list runs again through the real runner, which applies 000010
//! alone. The retired default reads `is_default` false, one version higher, its `updated_at` the
//! migration's instant, as a category write does; no audit row is written; every other row is
//! byte for byte what it was. An upgraded database equals a fresh one, a replay applies nothing,
//! and the gear's repository reads the cleared row. The Postgres twin is
//! `postgres_retired_default_migration.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_products::gear::BssProductsGear;
use bss_products::infra::storage::repo;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::secure::{AccessScope, DBRunner};
use toolkit_db::{ConnectOpts, DBProvider, connect_db};
use uuid::Uuid;

const MIGRATION: &str = "m20260928_000010_clear_retired_defaults";
const TABLE: &str = "products_category";
/// A tenant that retired its default before P-D-220.
const T1: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0010_0001);
/// A tenant whose default is active.
const T2: Uuid = Uuid::from_u128(0x7e0a_0000_0000_0000_0000_0000_0010_0002);
const RETIRED_DEFAULT: Uuid = Uuid::from_u128(0x0010_0001);
const T1_ACTIVE: Uuid = Uuid::from_u128(0x0010_0002);
const ACTIVE_DEFAULT: Uuid = Uuid::from_u128(0x0010_0003);
const T2_RETIRED: Uuid = Uuid::from_u128(0x0010_0004);
const STORED_AT: &str = "2026-09-27T09:00:00.123456Z";

/// A file database in its own temporary directory, so the runner's pool, the repository and a raw
/// connection see one schema, and the directory goes with the test.
struct Lite {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl Lite {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("products-retired-default-")
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

    /// Every category as a JSON object of all its columns (blobs as hex), keyed by its id.
    async fn rows(&self) -> Vec<(Uuid, Value)> {
        let columns = self
            .strings(&format!(
                "SELECT name AS v FROM pragma_table_info('{TABLE}') ORDER BY cid"
            ))
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
        let raw = self.raw().await;
        let rows = raw
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                format!("SELECT id, json_object({pairs}) AS v FROM {TABLE} ORDER BY id"),
            ))
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.try_get::<Uuid>("", "id").unwrap(),
                    serde_json::from_str(&row.try_get::<String>("", "v").unwrap()).unwrap(),
                )
            })
            .collect();
        raw.close().await.unwrap();
        rows
    }

    async fn audit_rows(&self) -> Vec<String> {
        self.strings("SELECT CAST(count(*) AS TEXT) AS v FROM products_audit_log")
            .await
    }
}

/// The deployed database before this run: the chain without 000010, and four categories written
/// with the values the application binds.
async fn seeded() -> Lite {
    let db = Lite::new();
    let before = db.migrate(Some(MIGRATION)).await.unwrap();
    assert!(
        !before.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    let at = OffsetDateTime::parse(STORED_AT, &Rfc3339).unwrap();
    let raw = db.raw().await;
    for (id, tenant, code, is_default, status, version) in [
        (RETIRED_DEFAULT, T1, "legacy", true, "retired", 3_i64),
        (T1_ACTIVE, T1, "hosting", false, "active", 1),
        (ACTIVE_DEFAULT, T2, "general", true, "active", 2),
        (T2_RETIRED, T2, "old", false, "retired", 2),
    ] {
        raw.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO products_category (id, tenant_id, code, name, is_default, sort_order, \
             status, version, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 0, ?, ?, ?, ?)",
            [
                id.into(),
                tenant.into(),
                code.into(),
                format!("Category {code}").into(),
                is_default.into(),
                status.into(),
                version.into(),
                at.into(),
                at.into(),
            ],
        ))
        .await
        .unwrap();
    }
    raw.close().await.unwrap();
    db
}

#[tokio::test]
async fn every_retired_default_is_cleared_and_every_other_category_kept() {
    let db = seeded().await;
    let structure_before = db.structure().await;
    let rows_before = db.rows().await;
    assert_eq!(rows_before.len(), 4, "{rows_before:#?}");
    let audit_before = db.audit_rows().await;
    // The migration's instant is at or after this second.
    let started = OffsetDateTime::now_utc().replace_nanosecond(0).unwrap();

    let result = db.migrate(None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000010 was pending");
    let finished = OffsetDateTime::now_utc();
    // Data only: the table's structure is what it was.
    assert_eq!(db.structure().await, structure_before);
    let rows_after = db.rows().await;
    assert_eq!(rows_after.len(), rows_before.len());
    for ((id, before), (id_after, after)) in rows_before.iter().zip(&rows_after) {
        assert_eq!(id, id_after);
        if *id != RETIRED_DEFAULT {
            assert_eq!(after, before, "{id} is kept byte for byte");
            continue;
        }
        // The retired default: cleared as a category write clears it.
        assert_eq!(after["is_default"], 0, "{after}");
        assert_eq!(after["version"], 4, "{after}");
        assert_eq!(after["status"], "retired", "{after}");
        let written =
            OffsetDateTime::parse(after["updated_at"].as_str().unwrap(), &Rfc3339).unwrap();
        assert!(
            started <= written && written <= finished,
            "{written} is the migration's instant"
        );
        let (mut kept_before, mut kept_after) = (before.clone(), after.clone());
        for column in ["is_default", "version", "updated_at"] {
            kept_before.as_object_mut().unwrap().remove(column);
            kept_after.as_object_mut().unwrap().remove(column);
        }
        assert_eq!(kept_after, kept_before, "every other column is kept");
    }
    assert!(
        rows_before
            .iter()
            .any(|(id, row)| *id == ACTIVE_DEFAULT && row["is_default"] == 1),
        "the active default was seeded as the default"
    );
    // A migration is no act of a user: it writes no audit row.
    assert_eq!(db.audit_rows().await, audit_before);
    assert_eq!(audit_before, ["0"]);
    // No retired default is left.
    assert_eq!(
        db.strings(&format!(
            "SELECT CAST(count(*) AS TEXT) AS v FROM {TABLE} WHERE is_default AND status = 'retired'"
        ))
        .await,
        ["0"]
    );

    // An upgraded database and a fresh one hold the same table.
    let fresh = Lite::new();
    fresh.migrate(None).await.unwrap();
    assert_eq!(fresh.structure().await, structure_before);
    // A replay applies nothing and changes nothing.
    assert!(db.migrate(None).await.unwrap().applied_names.is_empty());
    assert_eq!(db.rows().await, rows_after);
}

/// A tenant's categories as the gear's repository reads them: id, default flag, status, version.
async fn flags(conn: &impl DBRunner, tenant: Uuid) -> Vec<(Uuid, bool, String, i64)> {
    repo::list_categories(conn, &AccessScope::for_tenant(tenant), tenant)
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.id, c.is_default, c.status, c.version))
        .collect()
}

/// The gear's own repository reads the cleared category (its `updated_at` parses as the
/// application's own), and the tenant it belonged to holds no default.
#[tokio::test]
async fn the_gear_reads_the_cleared_category_and_its_tenant_holds_no_default() {
    let db = seeded().await;
    db.migrate(None).await.unwrap();
    let provider = DBProvider::<toolkit_db::DbError>::new(db.pool().await);
    let conn = provider.conn().unwrap();
    let mut t1 = flags(&conn, T1).await;
    t1.sort();
    assert_eq!(
        t1,
        [
            (RETIRED_DEFAULT, false, "retired".to_owned(), 4),
            (T1_ACTIVE, false, "active".to_owned(), 1),
        ]
    );
    let mut t2 = flags(&conn, T2).await;
    t2.sort();
    assert_eq!(
        t2,
        [
            (ACTIVE_DEFAULT, true, "active".to_owned(), 2),
            (T2_RETIRED, false, "retired".to_owned(), 2),
        ]
    );
    // T1 has no default to clear: a default move there clears nothing.
    let cleared = repo::clear_default_category(
        &conn,
        &AccessScope::for_tenant(T1),
        T1,
        Some(T1_ACTIVE),
        OffsetDateTime::now_utc(),
    )
    .await
    .unwrap();
    assert!(cleared.is_empty(), "{cleared:?}");
}
