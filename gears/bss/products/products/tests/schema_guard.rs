//! P-D-195 on `SQLite`: the chain refuses a legacy or stale products schema before it creates
//! anything, and lets a fresh database and today's database through.
//!
//! Every case drives the gear's real migration list through the toolkit runner (history table,
//! name sort, pending-only), on a file database a second, raw connection can shape first. The
//! Postgres twin is `postgres_schema_guard.rs`; pricing carries the same suite (D-423).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod guard_support;

use bss_products::gear::BssProductsGear;
use bss_products::infra::storage::migrations::m0000_products_refuse_a_legacy_or_stale_schema as guard;
use guard_support::{
    GUARD, LEGACY_CATEGORY_SQLITE, LEGACY_CHAIN_TABLES, LEGACY_SKU_SQLITE,
    SKU_REFERENCE_BEFORE_RENAME_SQLITE, STALE_REFERENCE, refusal,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::{ConnectOpts, connect_db};

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
            .prefix("products-guard-")
            .tempdir()
            .unwrap();
        let path = dir.path().join("db.sqlite3");
        Self { _dir: dir, path }
    }

    fn dsn(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }

    /// The gear's whole migration list through the toolkit runner, on its own pool.
    async fn migrate(&self) -> Result<MigrationResult, MigrationError> {
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
        run_migrations_for_testing(&db, BssProductsGear::default().migrations()).await
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
    }

    async fn tables(&self) -> Vec<String> {
        let raw = self.raw().await;
        raw.query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
             ORDER BY name"
                .to_owned(),
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "name").unwrap())
        .collect()
    }

    async fn products_tables(&self) -> Vec<String> {
        self.tables()
            .await
            .into_iter()
            .filter(|t| t.starts_with("products_"))
            .collect()
    }

    /// The runner's history table (`run_migrations_for_testing` names it after `_test`).
    async fn history(&self) -> String {
        let tables = self.tables().await;
        let mut ledgers = tables
            .iter()
            .filter(|t| t.starts_with("toolkit_migrations"));
        let ledger = ledgers
            .next()
            .expect("the runner created its history table")
            .clone();
        assert!(ledgers.next().is_none(), "one history table");
        ledger
    }

    /// Make `names` pending again, as on a database that predates them.
    async fn forget(&self, names: &[&str]) {
        let ledger = self.history().await;
        for name in names {
            self.exec(&[&format!(
                r#"DELETE FROM "{ledger}" WHERE version = '{name}'"#
            )])
            .await;
        }
    }
}

fn refused(result: Result<MigrationResult, MigrationError>) -> String {
    let error = result.expect_err("the guard refuses this database");
    let text = error.to_string();
    assert!(
        text.contains(&format!("migration '{GUARD}' failed")),
        "the guard itself refused, not a later migration: {text}"
    );
    text
}

/// The legacy chain's tables minus every table a fresh chain creates, measured here and not
/// read from the guard: a name the guard's constant lost is still refused-or-red below.
async fn legacy_set() -> Vec<String> {
    let db = Lite::new();
    db.migrate().await.unwrap();
    let fresh = db.tables().await;
    LEGACY_CHAIN_TABLES
        .iter()
        .filter(|t| !fresh.iter().any(|f| f == *t))
        .map(|t| (*t).to_owned())
        .collect()
}

/// (a) One legacy table, created before the chain ever ran, is refused by name — for every name
/// of the legacy set — and nothing of the gear is created.
#[tokio::test]
async fn every_legacy_table_alone_is_refused_before_the_gear_creates_anything() {
    let legacy_set = legacy_set().await;
    assert_eq!(legacy_set.len(), 35);
    for legacy in &legacy_set {
        let db = Lite::new();
        db.exec(&[&format!("CREATE TABLE {legacy} (id text PRIMARY KEY)")])
            .await;

        let result = db.migrate().await;
        assert!(result.is_err(), "the legacy table {legacy} was not refused");
        let text = refused(result);

        let want = refusal("legacy", &format!("table {legacy}"));
        assert!(text.contains(&want), "{legacy}: {text}\nwanted: {want}");
        assert_eq!(
            db.products_tables().await,
            vec![legacy.clone()],
            "the guard runs before every products migration"
        );
    }
}

/// (a) Several legacy tables are all named, sorted.
#[tokio::test]
async fn several_legacy_tables_are_named_together() {
    let db = Lite::new();
    db.exec(&[
        "CREATE TABLE products_product (id text)",
        "CREATE TABLE products_approval (id text)",
    ])
    .await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal(
            "legacy",
            "tables products_approval, products_product"
        )),
        "{text}"
    );
}

/// (b) Today's chain, then `products_sku_reference` rebuilt with the CHECK it had before the
/// phase 2 rename edited `m20260925_000006` in place, and the guard pending again.
#[tokio::test]
async fn a_reference_table_whose_check_refuses_price_book_entry_is_stale() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    db.exec(&["DROP TABLE products_sku_reference"]).await;
    db.exec(SKU_REFERENCE_BEFORE_RENAME_SQLITE).await;
    db.forget(&[GUARD]).await;

    let text = refused(db.migrate().await);

    assert!(text.contains(&refusal("stale", STALE_REFERENCE)), "{text}");
}

/// (b, phase 4 review F2) The legacy chain's `products_category` — a name today's chain creates
/// too — left behind by a clean-up that dropped only what the refusal named, with its migration
/// pending: `m20260925_000001`'s `CREATE TABLE IF NOT EXISTS` would keep it and its index on `code`
/// fail with a raw SQL error. The guard refuses it first.
#[tokio::test]
async fn a_legacy_category_without_code_is_stale() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    db.exec(&["DROP TABLE products_category"]).await;
    db.exec(LEGACY_CATEGORY_SQLITE).await;
    db.forget(&[GUARD, CATEGORY_MIGRATION]).await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal("stale", "products_category without column code")),
        "{text}"
    );
}

/// (b, phase 4 review F2) The legacy chain's `products_sku` (`sku_code`, no `code`), with its
/// migration pending: the guard's refusal, not `m20260925_000002`'s.
#[tokio::test]
async fn a_legacy_sku_without_code_is_stale() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    db.exec(&["DROP TABLE products_sku_version", "DROP TABLE products_sku"])
        .await;
    db.exec(LEGACY_SKU_SQLITE).await;
    db.forget(&[GUARD, SKU_MIGRATION]).await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal("stale", "products_sku without column code")),
        "{text}"
    );
}

/// The migrations that create the two tables both chains name.
const CATEGORY_MIGRATION: &str = "m20260925_000001_create_products_category";
const SKU_MIGRATION: &str = "m20260925_000002_create_products_sku";

/// (c) A fresh database passes, the guard first.
#[tokio::test]
async fn a_fresh_database_passes_with_the_guard_first() {
    let db = Lite::new();

    let result = db.migrate().await.unwrap();

    assert_eq!(result.applied_names[0], GUARD);
    assert_eq!(
        result.applied,
        BssProductsGear::default().migrations().len()
    );
}

/// (d) Today's chain applied, the guard pending again: it finds nothing and only it runs.
#[tokio::test]
async fn todays_database_with_the_guard_pending_passes() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    db.forget(&[GUARD]).await;

    let result = db.migrate().await.unwrap();

    assert_eq!(result.applied_names, vec![GUARD.to_owned()]);
}

/// (e, H1) No table a fresh chain creates can count as legacy: the guard's constant is exactly
/// the legacy chain's tables minus a fresh chain's, so it is disjoint from the fresh schema.
#[tokio::test]
async fn the_legacy_set_is_the_legacy_chain_minus_a_fresh_chain() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    let fresh = db.tables().await;

    assert_eq!(
        db.products_tables().await.len(),
        12,
        "the fresh chain's products tables"
    );
    let in_both: Vec<&str> = LEGACY_CHAIN_TABLES
        .iter()
        .copied()
        .filter(|t| fresh.iter().any(|f| f == t))
        .collect();
    assert_eq!(
        in_both,
        [
            "products_approval_decision",
            "products_audit_log",
            "products_category",
            "products_idempotency",
            "products_sku",
        ],
        "not evidence (H1)"
    );
    assert_eq!(
        guard::LEGACY_TABLES,
        legacy_set().await,
        "legacy = chain minus fresh"
    );
    for legacy in guard::LEGACY_TABLES {
        assert!(
            !fresh.iter().any(|t| t == legacy),
            "{legacy} is legacy and also created by today's chain"
        );
    }
}
