//! P-D-196 on `SQLite`: `m20260925_000007_sku_category_optional` makes `products_sku.category_id`
//! nullable by rebuilding the SKU family inside the toolkit runner's transaction.
//!
//! Products has no schema golden, so this comparison is the proof (plan review M8). A database is
//! migrated by the gear's whole list without 000007 — the chain the deployment runs today — then SKUs,
//! versions and references (live and released) are seeded, the structure is captured, and the
//! whole list runs again through the real runner (`run_migrations_for_testing`, one transaction per
//! migration, `foreign_keys` on), which applies 000007 alone. The structure captured is the
//! `sqlite_master` DDL of the three tables and of their indexes and triggers, and
//! `pragma table_info` and `pragma foreign_key_list` of the three tables. Exactly two facts may
//! differ: `category_id`'s NOT NULL in `table_info`, and the same clause in `products_sku`'s DDL.
//! Every row must survive. The Postgres twin is `postgres_sku_category_migration.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use bss_products::gear::BssProductsGear;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::{ConnectOpts, connect_db};

const MIGRATION: &str = "m20260925_000007_sku_category_optional";
/// 000011 rebuilds the same family again (P-D-248). This proof is about 000007 alone, so the
/// later rebuild stays out of both passes.
const LATER_REBUILD: &str = "m20261001_000011_sku_lifecycle_honesty";
/// 000013's triggers sit on `products_sku`. 000007 rebuilds that table, so a pass that
/// applied 000013 first would drop the triggers and look like a second structural change.
/// This proof is about 000007 alone, so those triggers stay out of both passes.
const DERIVED_UNIT: &str = "m20261002_000013_derived_sku_unit";
/// 000014's archive columns and partial index sit on `products_sku` too (P-D-263); 000007's
/// rebuild would drop them the same way, so they stay out of both passes as well.
const ARCHIVE_MARK: &str = "m20261003_000014_archive_mark";
const GUARD: &str = "m0000_products_refuse_a_legacy_or_stale_schema";
/// The parent first, then its two children.
const FAMILY: [&str; 3] = [
    "products_sku",
    "products_sku_version",
    "products_sku_reference",
];

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
            .prefix("products-category-migration-")
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
            .filter(|m| {
                Some(m.name()) != without
                    && m.name() != LATER_REBUILD
                    && m.name() != DERIVED_UNIT
                    && m.name() != ARCHIVE_MARK
            })
            .collect();
        run_migrations_for_testing(&db, chain).await
    }

    async fn raw(&self) -> DatabaseConnection {
        Database::connect(self.dsn()).await.unwrap()
    }

    async fn exec(&self, statements: &[&str]) {
        let raw = self.raw().await;
        for sql in statements {
            raw.execute_raw(Statement::from_string(DbBackend::Sqlite, (*sql).to_owned()))
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
        raw.close().await.unwrap();
    }

    async fn refused(&self, sql: &str) -> String {
        let raw = self.raw().await;
        let error = raw
            .execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await
            .expect_err(sql)
            .to_string();
        raw.close().await.unwrap();
        error
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

    /// The structure of the family, one fact per line, sorted. Table DDL has its double quotes
    /// removed: `ALTER TABLE … RENAME` writes the new name quoted, which is the same schema.
    async fn structure(&self) -> Vec<String> {
        let names = FAMILY.map(|t| format!("'{t}'")).join(",");
        let mut facts = self
            .strings(&format!(
                "SELECT type || ' ' || name || ' ON ' || tbl_name || ': ' || \
                 CASE WHEN type = 'table' THEN replace(sql, '\"', '') ELSE coalesce(sql, '(auto)') END AS v \
                 FROM sqlite_master WHERE tbl_name IN ({names})"
            ))
            .await;
        for table in FAMILY {
            facts.extend(
                self.strings(&format!(
                    "SELECT 'table_info {table}: ' || cid || ' ' || name || ' ' || type || \
                     ' notnull=' || \"notnull\" || ' dflt=' || coalesce(dflt_value, '(none)') || \
                     ' pk=' || pk AS v FROM pragma_table_info('{table}')"
                ))
                .await,
            );
            facts.extend(
                self.strings(&format!(
                    "SELECT 'foreign_key_list {table}: ' || id || ' ' || seq || ' ' || \"table\" || \
                     ' ' || \"from\" || ' ' || \"to\" || ' ' || on_update || ' ' || on_delete || ' ' || \
                     \"match\" AS v FROM pragma_foreign_key_list('{table}')"
                ))
                .await,
            );
        }
        facts.sort();
        facts
    }

    /// Every row of the family, each as JSON of all its columns, in key order.
    async fn rows(&self) -> Vec<String> {
        let mut rows = Vec::new();
        for table in FAMILY {
            let columns = self
                .strings(&format!(
                    "SELECT name AS v FROM pragma_table_info('{table}') ORDER BY cid"
                ))
                .await;
            let pairs = columns
                .iter()
                .map(|c| format!("'{c}', \"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            let key = if table == "products_sku_version" {
                "sku_id, published_version"
            } else {
                "id"
            };
            rows.extend(
                self.strings(&format!(
                    "SELECT '{table} ' || json_object({pairs}) AS v FROM {table} ORDER BY {key}"
                ))
                .await,
            );
        }
        rows
    }

    /// The runner's history table (`run_migrations_for_testing` names it after `_test`).
    async fn forget(&self, name: &str) {
        let ledger = self
            .strings(
                "SELECT name AS v FROM sqlite_master WHERE type = 'table' AND name LIKE 'toolkit_migrations%'",
            )
            .await;
        assert_eq!(ledger.len(), 1, "one history table: {ledger:?}");
        self.exec(&[&format!(
            r#"DELETE FROM "{}" WHERE version = '{name}'"#,
            ledger[0]
        )])
        .await;
    }
}

/// Two SKUs with every column set away from its default, two versions, and references in all
/// three states — a released one beside a live one under the same live key.
const SEED: &[&str] = &[
    "INSERT INTO products_category (id,tenant_id,code,name,is_default,status,created_at,updated_at) VALUES ('c1','t1','hosting','Hosting',1,'active','2026-09-25T00:00:00Z','2026-09-25T00:00:00Z')",
    "INSERT INTO products_sku (id,tenant_id,code,name,type,category_id,description,sellable,lifecycle,fence_prior_lifecycle,fenced_at,fence_op_id,revision,published_version,gl_code,tax_category,invoice_line_template,billing_timing,usage_type_ref,unit,type_change_pending,pending_unit_id,approved_by_unit_id,created_by,created_at,updated_at) VALUES ('s1','t1','STOR','Storage','usage','c1','Block storage',0,'published','published','2026-09-25T01:00:00Z','f1',4,2,'4010','T1','{name}','arrears','storage','GB',1,'u1','u0','a1','2026-09-25T00:00:00Z','2026-09-25T02:00:00Z')",
    "INSERT INTO products_sku (id,tenant_id,code,name,type,category_id,lifecycle,created_by,created_at,updated_at) VALUES ('s2','t1','SEAT','Seat','recurring','c1','draft','a2','2026-09-25T00:00:00Z','2026-09-25T00:00:00Z')",
    "INSERT INTO products_sku_version (sku_id,tenant_id,published_version,effective_from,content,created_at) VALUES ('s1','t1',1,'2026-09-25','{\"code\":\"STOR\"}','2026-09-25T00:00:00Z')",
    "INSERT INTO products_sku_version (sku_id,tenant_id,published_version,effective_from,content,created_at) VALUES ('s1','t1',2,'2026-10-01','{\"code\":\"STOR\",\"gl_code\":\"4010\"}','2026-09-25T02:00:00Z')",
    "INSERT INTO products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,reserved_by,reserved_at) VALUES ('r1','t1','s1','pricing','price_book_entry','e1','reserved','p1','2026-09-25T03:00:00Z')",
    "INSERT INTO products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,reserved_by,reserved_at,confirmed_at) VALUES ('r2','t1','s1','pricing','plan_item','i1','confirmed','p1','2026-09-25T03:00:00Z','2026-09-25T03:01:00Z')",
    "INSERT INTO products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,reserved_by,reserved_at,released_at,released_by,release_reason,forced) VALUES ('r3','t1','s1','pricing','price_book_entry','e1','released','p1','2026-09-25T02:00:00Z','2026-09-25T02:30:00Z','o1','abandoned',1)",
];

/// The clause 000007 changes, before and after.
const REQUIRED: &str = "category_id text NOT NULL REFERENCES products_category(id)";
const OPTIONAL: &str = "category_id text NULL REFERENCES products_category(id)";

#[tokio::test]
async fn the_family_rebuild_keeps_every_row_and_changes_only_the_category_not_null() {
    let db = Lite::new();
    let before_run = db.migrate(Some(MIGRATION)).await.unwrap();
    assert!(
        !before_run.applied_names.iter().any(|n| n == MIGRATION),
        "the chain before this run"
    );
    db.exec(SEED).await;
    let structure_before = db.structure().await;
    let rows_before = db.rows().await;
    assert_eq!(rows_before.len(), 7, "{rows_before:#?}");

    let result = db.migrate(None).await.unwrap();

    assert_eq!(result.applied_names, [MIGRATION], "only 000007 was pending");
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
        "P-D-196 SQLite proof: {} structural facts before, {} after, {} rows\nremoved:\n  {}\nadded:\n  {}",
        structure_before.len(),
        structure_after.len(),
        rows_before.len(),
        show(&removed),
        show(&added)
    );
    assert_eq!(removed.len(), 2, "removed: {removed:#?}\nadded: {added:#?}");
    assert_eq!(added.len(), 2, "removed: {removed:#?}\nadded: {added:#?}");
    let info = |facts: &[&String]| {
        facts
            .iter()
            .find(|f| f.starts_with("table_info products_sku: "))
            .map_or_else(
                || panic!("no table_info fact in {facts:#?}"),
                |f| (*f).clone(),
            )
    };
    assert_eq!(
        info(&removed),
        "table_info products_sku: 5 category_id TEXT notnull=1 dflt=(none) pk=0"
    );
    assert_eq!(
        info(&added),
        "table_info products_sku: 5 category_id TEXT notnull=0 dflt=(none) pk=0"
    );
    let ddl = |facts: &[&String]| {
        facts
            .iter()
            .find(|f| f.starts_with("table products_sku ON products_sku: "))
            .map_or_else(
                || panic!("no products_sku DDL in {facts:#?}"),
                |f| (*f).clone(),
            )
    };
    let (ddl_before, ddl_after) = (ddl(&removed), ddl(&added));
    assert_eq!(ddl_before.matches(REQUIRED).count(), 1, "{ddl_before}");
    assert_eq!(
        ddl_after,
        ddl_before.replace(REQUIRED, OPTIONAL),
        "only the category clause of the parent's DDL changes"
    );
    assert_eq!(db.rows().await, rows_before, "every row survives");
    assert!(
        db.strings("SELECT \"table\" AS v FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
}

/// What the structure promises, exercised: a SKU without a category is accepted, every foreign
/// key still points at the rebuilt parent, the append-only triggers and the live partial unique
/// index still refuse, and the schema guard still passes the rebuilt reference table.
#[tokio::test]
async fn after_the_rebuild_the_keys_triggers_and_live_index_still_hold() {
    let db = Lite::new();
    db.migrate(Some(MIGRATION)).await.unwrap();
    db.exec(SEED).await;
    db.migrate(None).await.unwrap();

    db.exec(&["INSERT INTO products_sku (id,tenant_id,code,name,type,category_id,lifecycle,created_by,created_at,updated_at) VALUES ('s3','t1','LOOSE','Loose','recurring',NULL,'draft','a3','2026-09-26T00:00:00Z','2026-09-26T00:00:00Z')"]).await;
    for (sql, refusal) in [
        (
            "INSERT INTO products_sku (id,tenant_id,code,name,type,category_id,lifecycle,created_by,created_at,updated_at) VALUES ('s4','t1','GONE','Gone','recurring','missing','draft','a4','2026-09-26T00:00:00Z','2026-09-26T00:00:00Z')",
            "FOREIGN KEY constraint failed",
        ),
        (
            "INSERT INTO products_sku_version (sku_id,tenant_id,published_version,effective_from,content,created_at) VALUES ('nope','t1',1,'2026-09-26','{}','2026-09-26T00:00:00Z')",
            "FOREIGN KEY constraint failed",
        ),
        (
            "INSERT INTO products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,reserved_by,reserved_at) VALUES ('r9','t1','nope','pricing','sold_as','x9','reserved','p1','2026-09-26T00:00:00Z')",
            "FOREIGN KEY constraint failed",
        ),
        ("DELETE FROM products_sku WHERE id = 's2'", ""),
        (
            "UPDATE products_sku_version SET content = '[]'",
            "products_sku_version is append-only",
        ),
        (
            "DELETE FROM products_sku_version",
            "products_sku_version is append-only",
        ),
        (
            "INSERT INTO products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,reserved_by,reserved_at) VALUES ('r4','t1','s1','pricing','price_book_entry','e1','reserved','p1','2026-09-26T00:00:00Z')",
            "UNIQUE constraint failed: products_sku_reference.tenant_id, products_sku_reference.owner_gear, products_sku_reference.ref_kind, products_sku_reference.ref_id",
        ),
        (
            "INSERT INTO products_sku_reference (id,tenant_id,sku_id,owner_gear,ref_kind,ref_id,state,reserved_by,reserved_at) VALUES ('r5','t1','s1','pricing','price','x5','reserved','p1','2026-09-26T00:00:00Z')",
            "CHECK constraint failed",
        ),
        (
            "DELETE FROM products_sku WHERE id = 's1'",
            "FOREIGN KEY constraint failed",
        ),
    ] {
        if refusal.is_empty() {
            db.exec(&[sql]).await;
            continue;
        }
        let error = db.refused(sql).await;
        assert!(error.contains(refusal), "{sql}\n{error}");
    }
    assert_eq!(
        db.strings("SELECT code AS v FROM products_sku WHERE category_id IS NULL")
            .await,
        ["LOOSE"]
    );

    db.forget(GUARD).await;
    let result = db.migrate(None).await.unwrap();
    assert_eq!(
        result.applied_names,
        [GUARD],
        "the guard passes the rebuilt family"
    );
}
