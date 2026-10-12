//! P-D-213 on `SQLite`: `m20260927_000008_audit_lifecycle_move` adds `from_lifecycle` and
//! `to_lifecycle` to `products_audit_log` and redefines the seal trigger.
//!
//! Products has no schema golden, so this comparison is the proof, read by eye in the run's report.
//! A database is migrated by the gear's whole list without 000008 — the chain the deployment runs today —
//! then audit rows are seeded (unsealed ones and one the platform already sealed), the structure of
//! the table is captured, and the whole list runs again through the real runner
//! (`run_migrations_for_testing`, one transaction per migration), which applies 000008 alone. The
//! structure is the `sqlite_master` DDL of the table, its indexes and its triggers, and `pragma
//! table_info`. Exactly these facts may differ: the table's DDL (the two column definitions after
//! the last one), the seal
//! trigger's DDL (the two columns added to its unchanged-list), and the two new `table_info` rows.
//! Every row survives and reads `NULL` for both columns; the guard still refuses every UPDATE that
//! is not the seal and every DELETE, and the seal still passes. The Postgres twin is
//! `postgres_audit_lifecycle_migration.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_products::gear::BssProductsGear;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::{ConnectOpts, connect_db};

const MIGRATION: &str = "m20260927_000008_audit_lifecycle_move";
/// What the guard answers every refused UPDATE and DELETE.
const REFUSED: &str = "products_audit_log is append-only";
const TABLE: &str = "products_audit_log";

/// A file database, so the runner's pool and a raw connection see the same schema.
struct Lite {
    /// The database's own temporary directory, removed with the `Lite` (the file, its `-wal` and
    /// its `-shm`).
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
}

impl Lite {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("products-audit-lifecycle-migration-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("db.sqlite3");
        Self { _dir: dir, path }
    }

    fn dsn(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }

    /// The gear's whole migration list through the toolkit runner, on its own pool; `without`
    /// leaves one migration out, as the deployed database's chain does before this run.
    async fn migrate(&self, without: Option<&str>) -> Result<MigrationResult, MigrationError> {
        let db = connect_db(
            &self.dsn(),
            ConnectOpts {
                max_conns: Some(1),
                min_conns: Some(1),
                ..ConnectOpts::default()
            },
        )
        .await
        .unwrap();
        let chain = BssProductsGear::default()
            .migrations()
            .into_iter()
            .filter(|m| Some(m.name()) != without)
            .collect();
        run_migrations_for_testing(&db, chain).await
    }

    async fn raw(&self) -> DatabaseConnection {
        Database::connect(self.dsn()).await.unwrap()
    }

    async fn exec(&self, sql: &str) -> Result<(), String> {
        let raw = self.raw().await;
        let result = raw
            .execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .map(|_| ())
            .map_err(|e| e.to_string());
        raw.close().await.unwrap();
        result
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

    /// Every row as JSON of `columns`, in key order.
    async fn rows(&self, columns: &[String]) -> Vec<String> {
        let pairs = columns
            .iter()
            .map(|c| format!("'{c}', \"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        self.strings(&format!(
            "SELECT json_object({pairs}) AS v FROM {TABLE} ORDER BY audit_id"
        ))
        .await
    }

    async fn columns(&self) -> Vec<String> {
        self.strings(&format!(
            "SELECT name AS v FROM pragma_table_info('{TABLE}') ORDER BY cid"
        ))
        .await
    }
}

/// Three rows the deployed database's chain wrote: two unsealed, one the platform sealed.
const SEED: &[&str] = &[
    "INSERT INTO products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,subject_revision,reason,written_at,seal_state) VALUES ('a1','t1','u1','sku.create','sku','s1',1,NULL,'2026-09-25T00:00:00Z','unsealed')",
    "INSERT INTO products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,reason,written_at,seal_state) VALUES ('a2','t1','u2','approval.rejected','approval_unit','n1','no','2026-09-25T01:00:00Z','unsealed')",
    "INSERT INTO products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,written_at,seal_state) VALUES ('a3','t1','u1','sku.unfence','sku','s1','2026-09-25T02:00:00Z','unsealed')",
    "UPDATE products_audit_log SET seal_state = 'sealed', chain_id = 'c1', seq = 0, row_hash = x'01' WHERE audit_id = 'a3'",
];

/// The table's last column definition as `000004` spells it, and the two this migration adds.
const LAST_COLUMN: &str = "row_hash          blob,";
const FROM: &str = "from_lifecycle text CONSTRAINT chk_products_audit_log_from_lifecycle CHECK (from_lifecycle IN ('draft','published','deprecated','retiring','retired'))";
const TO: &str = "to_lifecycle text CONSTRAINT chk_products_audit_log_to_lifecycle CHECK (to_lifecycle IN ('draft','published','deprecated','retiring','retired'))";

/// The seal's clauses as `000004` spells them, and the two this migration adds after them.
const SEAL_TAIL: &str = "AND NEW.ceremony_ref IS OLD.ceremony_ref\n        )";
const SEAL_TAIL_AFTER: &str = "AND NEW.ceremony_ref IS OLD.ceremony_ref\n            AND NEW.from_lifecycle IS OLD.from_lifecycle\n            AND NEW.to_lifecycle IS OLD.to_lifecycle\n        )";

#[tokio::test]
async fn the_two_columns_arrive_empty_and_the_guard_keeps_them() {
    let db = Lite::new();
    let before_run = db.migrate(Some(MIGRATION)).await.unwrap();
    assert!(
        !before_run.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    for sql in SEED {
        db.exec(sql).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    let columns_before = db.columns().await;
    let structure_before = db.structure().await;
    let rows_before = db.rows(&columns_before).await;
    assert_eq!(rows_before.len(), 3, "{rows_before:#?}");

    let result = db.migrate(None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000008 was pending");
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
        "P-D-213 SQLite proof: {} structural facts before, {} after, {} rows\nremoved:\n  {}\nadded:\n  {}",
        structure_before.len(),
        structure_after.len(),
        rows_before.len(),
        show(&removed),
        show(&added)
    );
    let fact = |facts: &[&String], prefix: &str| {
        let found: Vec<String> = facts
            .iter()
            .filter(|f| f.starts_with(prefix))
            .map(|f| (*f).clone())
            .collect();
        assert_eq!(found.len(), 1, "one `{prefix}` fact in {facts:#?}");
        found[0].clone()
    };
    assert_eq!(removed.len(), 2, "removed: {removed:#?}\nadded: {added:#?}");
    assert_eq!(added.len(), 4, "removed: {removed:#?}\nadded: {added:#?}");
    // The table: `ADD COLUMN` writes each column definition after the last one, before the table
    // constraints; nothing else of the DDL changes.
    let (table_before, table_after) = (
        fact(&removed, "table products_audit_log: "),
        fact(&added, "table products_audit_log: "),
    );
    assert_eq!(
        table_before.matches(LAST_COLUMN).count(),
        1,
        "{table_before}"
    );
    assert_eq!(
        table_after,
        table_before.replace(LAST_COLUMN, &format!("{LAST_COLUMN} {FROM}, {TO},")),
    );
    // The seal trigger: the two columns added to its unchanged-list, nothing else changed.
    let (seal_before, seal_after) = (
        fact(&removed, "trigger trg_products_audit_log_seal_unchanged: "),
        fact(&added, "trigger trg_products_audit_log_seal_unchanged: "),
    );
    assert_eq!(seal_before.matches(SEAL_TAIL).count(), 1, "{seal_before}");
    assert_eq!(seal_after, seal_before.replace(SEAL_TAIL, SEAL_TAIL_AFTER));
    let n = columns_before.len();
    assert_eq!(
        fact(&added, &format!("table_info: {n} ")),
        format!("table_info: {n} from_lifecycle TEXT notnull=0 dflt=(none) pk=0")
    );
    assert_eq!(
        fact(&added, &format!("table_info: {} ", n + 1)),
        format!(
            "table_info: {} to_lifecycle TEXT notnull=0 dflt=(none) pk=0",
            n + 1
        )
    );

    // Every row survives, and a row written before the migration reads null for both.
    assert_eq!(db.rows(&columns_before).await, rows_before);
    assert_eq!(
        db.strings(&format!(
            "SELECT audit_id || ' ' || coalesce(from_lifecycle, 'null') || ' ' || \
             coalesce(to_lifecycle, 'null') AS v FROM {TABLE} ORDER BY audit_id"
        ))
        .await,
        ["a1 null null", "a2 null null", "a3 null null"]
    );

    // The guard: every UPDATE that is not the seal and every DELETE is refused; the seal passes
    // only while it keeps every record column, the two new ones included.
    for sql in [
        "UPDATE products_audit_log SET from_lifecycle = 'draft' WHERE audit_id = 'a1'",
        "UPDATE products_audit_log SET to_lifecycle = 'draft' WHERE audit_id = 'a1'",
        "UPDATE products_audit_log SET action = 'rewritten' WHERE audit_id = 'a1'",
        "UPDATE products_audit_log SET from_lifecycle = 'draft' WHERE audit_id = 'a3'",
        "UPDATE products_audit_log SET seal_state = 'sealed', chain_id = 'c1', seq = 1, row_hash = x'02', from_lifecycle = 'draft' WHERE audit_id = 'a1'",
        "UPDATE products_audit_log SET seal_state = 'sealed', chain_id = 'c1', seq = 1, row_hash = x'02', to_lifecycle = 'retired' WHERE audit_id = 'a1'",
        "DELETE FROM products_audit_log WHERE audit_id = 'a2'",
        "DELETE FROM products_audit_log WHERE audit_id = 'a3'",
    ] {
        let error = db.exec(sql).await.expect_err(sql);
        assert!(error.contains(REFUSED), "{sql}\n{error}");
    }
    db.exec(
        "UPDATE products_audit_log SET seal_state = 'sealed', chain_id = 'c1', seq = 1, \
         row_hash = x'02', prev_hash = x'01' WHERE audit_id = 'a1'",
    )
    .await
    .unwrap();
    db.exec(
        "INSERT INTO products_audit_log (audit_id,tenant_id,actor_ref,action,subject_kind,subject_id,written_at,seal_state,from_lifecycle,to_lifecycle) \
         VALUES ('a4','t1','u1','sku.fence_expired','sku','s1','2026-09-27T00:00:00Z','unsealed','retiring','published')",
    )
    .await
    .unwrap();
    assert_eq!(
        db.strings(&format!(
            "SELECT audit_id || ' ' || seal_state || ' ' || coalesce(from_lifecycle, 'null') || \
             ' ' || coalesce(to_lifecycle, 'null') AS v FROM {TABLE} ORDER BY audit_id"
        ))
        .await,
        [
            "a1 sealed null null",
            "a2 unsealed null null",
            "a3 sealed null null",
            "a4 unsealed retiring published"
        ]
    );
}
