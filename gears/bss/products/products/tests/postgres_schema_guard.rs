//! P-D-195 on Postgres: the same cases as `schema_guard.rs`, with the tables in schema `bss`,
//! read from `information_schema` and `pg_constraint` by the guard.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod guard_support;
mod pg_support;

use bss_products::gear::BssProductsGear;
use guard_support::{
    GUARD, LEGACY_CATEGORY_PG, LEGACY_CHAIN_TABLES, LEGACY_SKU_PG, SKU_REFERENCE_BEFORE_RENAME_PG,
    STALE_REFERENCE, refusal,
};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};

/// The gear's whole list, as the runtime runs it (`Pg::applied` runs the `Migrator` alone).
async fn migrate(pg: &Pg) -> Result<MigrationResult, MigrationError> {
    let db = pg.db().await;
    run_migrations_for_testing(&db, BssProductsGear::default().migrations()).await
}

async fn exec(pg: &Pg, statements: &[&str]) {
    let raw = pg.raw().await;
    for sql in statements {
        raw.execute_raw(Statement::from_string(
            DbBackend::Postgres,
            (*sql).to_owned(),
        ))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
}

async fn strings(pg: &Pg, sql: &str) -> Vec<String> {
    pg.raw()
        .await
        .query_all_raw(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<String>("", "v").unwrap())
        .collect()
}

/// Every `products_*` table, in either schema, so one created in the wrong place still counts.
async fn products_tables(pg: &Pg) -> Vec<String> {
    strings(
        pg,
        r"SELECT tablename::text AS v FROM pg_tables
          WHERE schemaname IN ('bss', 'public') AND tablename LIKE 'products\_%' ORDER BY 1",
    )
    .await
}

async fn forget(pg: &Pg, names: &[&str]) {
    let ledgers = strings(
        pg,
        r"SELECT schemaname || '.' || quote_ident(tablename) AS v FROM pg_tables
          WHERE tablename LIKE 'toolkit\_migrations%'",
    )
    .await;
    assert_eq!(ledgers.len(), 1, "one history table: {ledgers:?}");
    for name in names {
        exec(
            pg,
            &[&format!(
                "DELETE FROM {} WHERE version = '{name}'",
                ledgers[0]
            )],
        )
        .await;
    }
}

fn refused(result: Result<MigrationResult, MigrationError>) -> String {
    let text = result
        .expect_err("the guard refuses this database")
        .to_string();
    assert!(
        text.contains(&format!("migration '{GUARD}' failed")),
        "the guard itself refused, not a later migration: {text}"
    );
    text
}

/// (a) Every legacy table alone (the legacy chain's tables minus a fresh chain's, measured here),
/// in schema `bss` before the chain ever ran, is refused by name, and nothing of the gear is
/// created.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn every_legacy_table_alone_is_refused_before_the_gear_creates_anything() {
    let fresh = Pg::empty().await;
    migrate(&fresh).await.unwrap();
    let fresh = products_tables(&fresh).await;
    let legacy_set: Vec<&str> = LEGACY_CHAIN_TABLES
        .iter()
        .copied()
        .filter(|t| !fresh.iter().any(|f| f == t))
        .collect();
    assert_eq!(legacy_set.len(), 35);
    for legacy in legacy_set {
        let pg = Pg::empty().await;
        exec(
            &pg,
            &[
                "CREATE SCHEMA IF NOT EXISTS bss",
                &format!("CREATE TABLE bss.{legacy} (id uuid PRIMARY KEY)"),
            ],
        )
        .await;

        let result = migrate(&pg).await;
        assert!(result.is_err(), "the legacy table {legacy} was not refused");
        let text = refused(result);

        let want = refusal("legacy", &format!("table {legacy}"));
        assert!(text.contains(&want), "{legacy}: {text}\nwanted: {want}");
        assert_eq!(products_tables(&pg).await, vec![legacy.to_owned()]);
    }
}

/// (b) `products_sku_reference` with the CHECK it had before the phase 2 rename.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_reference_table_whose_check_refuses_price_book_entry_is_stale() {
    let pg = Pg::empty().await;
    migrate(&pg).await.unwrap();
    exec(&pg, &["DROP TABLE bss.products_sku_reference CASCADE"]).await;
    exec(&pg, SKU_REFERENCE_BEFORE_RENAME_PG).await;
    forget(&pg, &[GUARD]).await;

    let text = refused(migrate(&pg).await);

    assert!(text.contains(&refusal("stale", STALE_REFERENCE)), "{text}");
}

/// (b, phase 4 review F2) The legacy chain's `products_category` (no `code`) with its migration
/// pending: the guard's refusal, not `m20260925_000001`'s raw index error.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn a_legacy_category_without_code_is_stale() {
    let pg = Pg::applied().await;
    exec(&pg, &["DROP TABLE bss.products_category CASCADE"]).await;
    exec(&pg, LEGACY_CATEGORY_PG).await;
    forget(&pg, &[GUARD, "m20260925_000001_create_products_category"]).await;

    let text = refused(migrate(&pg).await);

    assert!(
        text.contains(&refusal("stale", "products_category without column code")),
        "{text}"
    );
}

/// (b, phase 4 review F2) The legacy chain's `products_sku` (`sku_code`, no `code`) with its
/// migration pending: the guard's refusal, not `m20260925_000002`'s.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn a_legacy_sku_without_code_is_stale() {
    let pg = Pg::applied().await;
    exec(
        &pg,
        &[
            "DROP TABLE bss.products_sku_version CASCADE",
            "DROP TABLE bss.products_sku CASCADE",
        ],
    )
    .await;
    exec(&pg, LEGACY_SKU_PG).await;
    forget(&pg, &[GUARD, "m20260925_000002_create_products_sku"]).await;

    let text = refused(migrate(&pg).await);

    assert!(
        text.contains(&refusal("stale", "products_sku without column code")),
        "{text}"
    );
}

/// (c) A fresh database passes, the guard first.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_fresh_database_passes_with_the_guard_first() {
    let pg = Pg::empty().await;

    let result = migrate(&pg).await.unwrap();

    assert_eq!(result.applied_names[0], GUARD);
    assert_eq!(
        result.applied,
        BssProductsGear::default().migrations().len()
    );
}

/// (d) Today's chain applied, the guard pending again: it finds nothing and only it runs.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn todays_database_with_the_guard_pending_passes() {
    let pg = Pg::empty().await;
    migrate(&pg).await.unwrap();
    forget(&pg, &[GUARD]).await;

    let result = migrate(&pg).await.unwrap();

    assert_eq!(result.applied_names, vec![GUARD.to_owned()]);
}
