//! D-423 on Postgres: the same cases as `schema_guard.rs`, with the tables in schema `bss`, read
//! from `information_schema` by the guard.
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod guard_support;
mod pg_support;

use bss_pricing::module::BssPricingGear;
use guard_support::{
    ENTRY_SHAPED_PRICE_PG, GUARD, LEGACY_CHAIN_TABLES, LEGACY_PLAN_PG, PLAN_MIGRATIONS,
    PRE_RENAME_FINDINGS, PRE_RENAME_NAMES, PRICE_ROW_PG, REFERENCE_OP_BEFORE_D412_PG,
    REFERENCE_OP_BEFORE_RENAME_PG, RENAMED, refusal,
};
use pg_support::Pg;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use toolkit::contracts::DatabaseCapability;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};

async fn migrate(pg: &Pg) -> Result<MigrationResult, MigrationError> {
    let db = pg.db().await;
    run_migrations_for_testing(&db, BssPricingGear::default().migrations()).await
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

/// Every `pricing_*` table, in either schema, so one created in the wrong place still counts.
async fn pricing_tables(pg: &Pg) -> Vec<String> {
    strings(
        pg,
        r"SELECT tablename::text AS v FROM pg_tables
          WHERE schemaname IN ('bss', 'public') AND tablename LIKE 'pricing\_%' ORDER BY 1",
    )
    .await
}

async fn history(pg: &Pg) -> String {
    let ledgers = strings(
        pg,
        r"SELECT schemaname || '.' || quote_ident(tablename) AS v FROM pg_tables
          WHERE tablename LIKE 'toolkit\_migrations%'",
    )
    .await;
    assert_eq!(ledgers.len(), 1, "one history table: {ledgers:?}");
    ledgers[0].clone()
}

async fn forget(pg: &Pg, names: &[&str]) {
    let ledger = history(pg).await;
    for name in names {
        exec(
            pg,
            &[&format!("DELETE FROM {ledger} WHERE version = '{name}'")],
        )
        .await;
    }
}

async fn remember(pg: &Pg, names: &[&str]) {
    let ledger = history(pg).await;
    for name in names {
        exec(
            pg,
            &[&format!("INSERT INTO {ledger} (version) VALUES ('{name}')")],
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
#[ignore = "needs the Postgres harness"]
async fn every_legacy_table_alone_is_refused_before_the_gear_creates_anything() {
    let fresh = Pg::empty().await;
    migrate(&fresh).await.unwrap();
    let fresh = pricing_tables(&fresh).await;
    let legacy_set: Vec<&str> = LEGACY_CHAIN_TABLES
        .iter()
        .copied()
        .filter(|t| !fresh.iter().any(|f| f == t))
        .collect();
    assert_eq!(legacy_set.len(), 45);
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
        assert_eq!(pricing_tables(&pg).await, vec![legacy.to_owned()]);
    }
}

/// (b) `pricing_reference_op` as it was before D-412.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn a_reference_op_without_ref_kind_is_stale() {
    let pg = Pg::applied().await;
    exec(&pg, &["DROP TABLE bss.pricing_reference_op CASCADE"]).await;
    exec(&pg, REFERENCE_OP_BEFORE_D412_PG).await;
    forget(&pg, &[GUARD]).await;

    let text = refused(migrate(&pg).await);

    assert!(
        text.contains(&refusal(
            "stale",
            "pricing_reference_op without column ref_kind"
        )),
        "{text}"
    );
}

/// (b) The pre-rename `pricing_price_row` beside today's tables.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn a_price_row_table_is_stale() {
    let pg = Pg::applied().await;
    exec(&pg, PRICE_ROW_PG).await;
    forget(&pg, &[GUARD]).await;

    let text = refused(migrate(&pg).await);

    assert!(
        text.contains(&refusal("stale", "table pricing_price_row")),
        "{text}"
    );
}

/// (b) `pricing_price` in its pre-rename, entry-shaped form.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn an_entry_shaped_price_is_stale() {
    let pg = Pg::applied().await;
    exec(&pg, &["DROP TABLE bss.pricing_price CASCADE"]).await;
    exec(&pg, ENTRY_SHAPED_PRICE_PG).await;
    forget(&pg, &[GUARD]).await;

    let text = refused(migrate(&pg).await);

    assert!(
        text.contains(&refusal(
            "stale",
            "pricing_price without column price_book_entry_id"
        )),
        "{text}"
    );
}

/// (b, M2) A pre-rename database with 000005, 000007 and the phase 3 migrations pending together
/// with the guard: the guard's refusal comes first and nothing pending ran.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn a_pre_rename_database_meets_the_guard_before_the_renamed_migrations() {
    let pg = Pg::applied().await;
    exec(
        &pg,
        &[
            "DROP TABLE bss.pricing_plan_item CASCADE",
            "DROP TABLE bss.pricing_plan_revision CASCADE",
            "DROP TABLE bss.pricing_plan CASCADE",
            "DROP TABLE bss.pricing_price CASCADE",
            "DROP TABLE bss.pricing_reference_op CASCADE",
            "DROP TABLE bss.pricing_price_book_entry CASCADE",
        ],
    )
    .await;
    exec(&pg, ENTRY_SHAPED_PRICE_PG).await;
    exec(&pg, REFERENCE_OP_BEFORE_RENAME_PG).await;
    exec(&pg, PRICE_ROW_PG).await;
    forget(&pg, &[GUARD]).await;
    forget(&pg, &RENAMED).await;
    forget(&pg, &PLAN_MIGRATIONS).await;
    remember(&pg, &PRE_RENAME_NAMES).await;

    let text = refused(migrate(&pg).await);

    assert!(
        text.contains(&refusal("stale", PRE_RENAME_FINDINGS)),
        "{text}"
    );
    let tables = pricing_tables(&pg).await;
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

/// (b, phase 4 review F2) The legacy chain's `pricing_plan` (no `code`) with the phase 3
/// migrations pending: the guard's refusal, not `m20260926_000010`'s raw index error.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn a_legacy_plan_without_code_is_stale() {
    let pg = Pg::applied().await;
    exec(
        &pg,
        &[
            "DROP TABLE bss.pricing_plan_item CASCADE",
            "DROP TABLE bss.pricing_plan_revision CASCADE",
            "DROP TABLE bss.pricing_plan CASCADE",
        ],
    )
    .await;
    exec(&pg, LEGACY_PLAN_PG).await;
    forget(&pg, &[GUARD]).await;
    forget(&pg, &PLAN_MIGRATIONS).await;

    let text = refused(migrate(&pg).await);

    assert!(
        text.contains(&refusal("stale", "pricing_plan without column code")),
        "{text}"
    );
    let tables = pricing_tables(&pg).await;
    for absent in ["pricing_plan_revision", "pricing_plan_item"] {
        assert!(
            !tables.iter().any(|t| t == absent),
            "{absent} was created: a pending migration ran before the guard"
        );
    }
}

/// (c) A fresh database passes, the guard first.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn a_fresh_database_passes_with_the_guard_first() {
    let pg = Pg::empty().await;

    let result = migrate(&pg).await.unwrap();

    assert_eq!(result.applied_names[0], GUARD);
    assert_eq!(result.applied, BssPricingGear::default().migrations().len());
}

/// (d) Today's chain applied, the guard pending again: it finds nothing and only it runs.
#[tokio::test]
#[ignore = "needs the Postgres harness"]
async fn todays_database_with_the_guard_pending_passes() {
    let pg = Pg::applied().await;
    forget(&pg, &[GUARD]).await;

    let result = migrate(&pg).await.unwrap();

    assert_eq!(result.applied_names, vec![GUARD.to_owned()]);
}
