//! D-423 on `SQLite`: the chain refuses a legacy or stale pricing schema before it creates
//! anything, and lets a fresh database and today's database through.
//!
//! Every case drives the gear's real migration list through the toolkit runner (history table,
//! name sort, pending-only), on a file database a second, raw connection can shape first. The
//! Postgres twin is `postgres_schema_guard.rs`.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod guard_support;

use bss_pricing::infra::storage::migrations::m0000_pricing_refuse_a_legacy_or_stale_schema as guard;
use bss_pricing::module::BssPricingGear;
use guard_support::{
    ENTRY_SHAPED_PRICE_SQLITE, GUARD, LEGACY_CHAIN_TABLES, LEGACY_PLAN_SQLITE, PLAN_MIGRATIONS,
    PRE_RENAME_FINDINGS, PRE_RENAME_NAMES, PRICE_ROW_SQLITE, REFERENCE_OP_BEFORE_D412_SQLITE,
    REFERENCE_OP_BEFORE_RENAME_SQLITE, RENAMED, refusal,
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
            .prefix("pricing-guard-")
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
        run_migrations_for_testing(&db, BssPricingGear::default().migrations()).await
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

    async fn pricing_tables(&self) -> Vec<String> {
        self.tables()
            .await
            .into_iter()
            .filter(|t| t.starts_with("pricing_"))
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

    /// Record `names` as applied, as a database that ran them does.
    async fn remember(&self, names: &[&str]) {
        let ledger = self.history().await;
        for name in names {
            self.exec(&[&format!(
                r#"INSERT INTO "{ledger}" (version) VALUES ('{name}')"#
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
    assert_eq!(legacy_set.len(), 45);
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
            db.pricing_tables().await,
            vec![legacy.clone()],
            "the guard runs before every pricing migration"
        );
    }
}

/// (a) Several legacy tables are all named, sorted.
#[tokio::test]
async fn several_legacy_tables_are_named_together() {
    let db = Lite::new();
    db.exec(&[
        "CREATE TABLE pricing_plan_phase (id text)",
        "CREATE TABLE pricing_bundle (id text)",
    ])
    .await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal(
            "legacy",
            "tables pricing_bundle, pricing_plan_phase"
        )),
        "{text}"
    );
}

/// (b) Today's chain, then `pricing_reference_op` rebuilt as it was before D-412, and the guard
/// pending again.
#[tokio::test]
async fn a_reference_op_without_ref_kind_is_stale() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    db.exec(&["DROP TABLE pricing_reference_op"]).await;
    db.exec(REFERENCE_OP_BEFORE_D412_SQLITE).await;
    db.forget(&[GUARD]).await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal(
            "stale",
            "pricing_reference_op without column ref_kind"
        )),
        "{text}"
    );
}

/// (b) Today's chain, then the pre-rename `pricing_price_row` beside it.
#[tokio::test]
async fn a_price_row_table_is_stale() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    db.exec(PRICE_ROW_SQLITE).await;
    db.forget(&[GUARD]).await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal("stale", "table pricing_price_row")),
        "{text}"
    );
}

/// (b) Today's chain, then `pricing_price` rebuilt in its pre-rename, entry-shaped form.
#[tokio::test]
async fn an_entry_shaped_price_is_stale() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    db.exec(&["DROP TABLE pricing_price"]).await;
    db.exec(ENTRY_SHAPED_PRICE_SQLITE).await;
    db.forget(&[GUARD]).await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal(
            "stale",
            "pricing_price without column price_book_entry_id"
        )),
        "{text}"
    );
}

/// (b, M2) A database migrated before the rename: the renamed migrations 000005 and 000007 and
/// the phase 3 ones are pending together with the guard. The guard sorts first, so the operator
/// reads D-423's refusal, not 000007's "no such column", and nothing pending ran.
#[tokio::test]
async fn a_pre_rename_database_meets_the_guard_before_the_renamed_migrations() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    db.exec(&[
        "DROP TABLE pricing_plan_item",
        "DROP TABLE pricing_plan_revision",
        "DROP TABLE pricing_plan",
        "DROP TABLE pricing_price",
        "DROP TABLE pricing_reference_op",
        "DROP TABLE pricing_price_book_entry",
    ])
    .await;
    db.exec(ENTRY_SHAPED_PRICE_SQLITE).await;
    db.exec(REFERENCE_OP_BEFORE_RENAME_SQLITE).await;
    db.exec(PRICE_ROW_SQLITE).await;
    db.forget(&[GUARD]).await;
    db.forget(&RENAMED).await;
    db.forget(&PLAN_MIGRATIONS).await;
    db.remember(&PRE_RENAME_NAMES).await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal("stale", PRE_RENAME_FINDINGS)),
        "{text}"
    );
    let tables = db.pricing_tables().await;
    for absent in [
        "pricing_price_book_entry",
        "pricing_plan",
        "pricing_plan_revision",
        "pricing_plan_item",
    ] {
        assert!(
            !tables.iter().any(|t| t == absent),
            "{absent} was created: a pending migration ran before the guard"
        );
    }
}

/// (b, phase 4 review F2) The legacy chain's `pricing_plan` — a name today's chain creates too, so
/// no legacy census holds it — left behind by a clean-up that dropped only what the refusal named,
/// with the phase 3 migrations pending: `m20260926_000010`'s `CREATE TABLE IF NOT EXISTS` would
/// keep it and its index on `code` fail with a raw SQL error. The guard refuses it first.
#[tokio::test]
async fn a_legacy_plan_without_code_is_stale() {
    let db = Lite::new();
    db.migrate().await.unwrap();
    db.exec(&[
        "DROP TABLE pricing_plan_item",
        "DROP TABLE pricing_plan_revision",
        "DROP TABLE pricing_plan",
    ])
    .await;
    db.exec(LEGACY_PLAN_SQLITE).await;
    db.forget(&[GUARD]).await;
    db.forget(&PLAN_MIGRATIONS).await;

    let text = refused(db.migrate().await);

    assert!(
        text.contains(&refusal("stale", "pricing_plan without column code")),
        "{text}"
    );
    let tables = db.pricing_tables().await;
    for absent in ["pricing_plan_revision", "pricing_plan_item"] {
        assert!(
            !tables.iter().any(|t| t == absent),
            "{absent} was created: a pending migration ran before the guard"
        );
    }
}

/// (c) A fresh database passes, the guard first, and it creates nothing of its own.
#[tokio::test]
async fn a_fresh_database_passes_with_the_guard_first() {
    let db = Lite::new();

    let result = db.migrate().await.unwrap();

    assert_eq!(result.applied_names[0], GUARD);
    assert_eq!(result.applied, BssPricingGear::default().migrations().len());
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
        db.pricing_tables().await.len(),
        19,
        "the fresh chain's pricing tables"
    );
    let in_both: Vec<&str> = LEGACY_CHAIN_TABLES
        .iter()
        .copied()
        .filter(|t| fresh.iter().any(|f| f == t))
        .collect();
    assert_eq!(
        in_both,
        ["pricing_plan", "pricing_price"],
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
