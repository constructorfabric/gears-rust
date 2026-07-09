//! C1 regression: the gear's migrations run through the toolkit runner
//! (`run_migrations_for_testing`), whose per-gear bookkeeping table is created
//! *unqualified*, BEFORE our schema migration creates `bss`. The connection
//! carries a `search_path` (prod config), so the schema that the unqualified
//! bookkeeping table resolves into depends on the path ORDER:
//!
//! - `bss,public` (the original, buggy order): on boot 1 `bss` does not exist
//!   yet, so bookkeeping lands in `public`; on boot 2 `bss` exists and is first
//!   in the path, so a *second* empty bookkeeping table is created in `bss`,
//!   the history reads empty, every migration re-runs, and the non-`IF NOT
//!   EXISTS` `CREATE TABLE bss.ledger_journal_entry` aborts -> startup crash loop.
//! - `public,bss` (the fix): `public` always exists and is first, so the
//!   bookkeeping table is in `public` on every boot; the history is stable and
//!   the second boot is a clean no-op. Domain tables are `bss.`-qualified in
//!   the migrations, so runtime DML still resolves them in `bss`.
//!
//! Ignored by default (Docker/testcontainers); run with
//! `cargo test -p bss-ledger --test postgres_migration_idempotency -- --ignored`.

#![allow(
    clippy::non_ascii_literal,
    clippy::let_underscore_must_use,
    clippy::needless_collect,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::doc_markdown
)]

use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, connect_db};

use bss_ledger::infra::storage::migrations::Migrator;

fn pg(sql: impl Into<String>) -> Statement {
    Statement::from_string(sea_orm::DatabaseBackend::Postgres, sql.into())
}

/// `SELECT count(*) ...` -> the `i64` count.
async fn count(db: &DatabaseConnection, sql: impl Into<String>) -> i64 {
    let row = db
        .query_one(pg(sql))
        .await
        .unwrap()
        .expect("count query must return a row");
    row.try_get::<i64>("", "count").unwrap()
}

/// Build a connection URL that sets `search_path` as a libpq option (the prod
/// gear config sets it per-connection the same way).
fn url_with_search_path(port: u16, search_path: &str) -> String {
    // `-c search_path=<value>` url-encoded: space -> %20, `=` -> %3D.
    format!(
        "postgres://postgres:postgres@127.0.0.1:{port}/postgres?options=-c%20search_path%3D{search_path}"
    )
}

/// The fix: with `public` first the toolkit runner is idempotent across boots,
/// the bookkeeping table lives in `public`, and the domain tables live in
/// `bss` only (so runtime DML resolves them there with no `public` collision).
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn migrations_idempotent_under_public_first_search_path() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();

    let db = connect_db(
        &url_with_search_path(port, "public,bss"),
        ConnectOpts::default(),
    )
    .await
    .expect("connect with public,bss search_path");

    // Boot 1: a fresh DB applies the whole chain.
    let r1 = run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("boot 1 migrations must succeed");
    assert_eq!(r1.applied, 45, "boot 1 applies all 45 migrations");

    // Boot 2: the same connection must skip everything — no re-create crash.
    let r2 = run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("boot 2 migrations must be a clean no-op (this is the C1 fix)");
    assert_eq!(r2.applied, 0, "boot 2 applies nothing");
    assert_eq!(r2.skipped, 45, "boot 2 skips all 45");

    // Inspect placement on a plain connection (information_schema is global).
    let base = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let raw = Database::connect(&base).await.unwrap();

    // Bookkeeping table must be in `public`, and there must be exactly one of
    // it (no duplicate in `bss`).
    let bk_total = count(
        &raw,
        "SELECT count(*) AS count FROM information_schema.tables \
         WHERE table_name LIKE 'toolkit_migrations%'",
    )
    .await;
    assert_eq!(bk_total, 1, "exactly one bookkeeping table");
    let bk_in_public = count(
        &raw,
        "SELECT count(*) AS count FROM information_schema.tables \
         WHERE table_name LIKE 'toolkit_migrations%' AND table_schema = 'public'",
    )
    .await;
    assert_eq!(bk_in_public, 1, "bookkeeping table lives in public");

    // Domain table is in `bss`, and NOT in `public` — so runtime DML (which is
    // unqualified) resolves it in `bss`, and the public-collision risk of the
    // `public,bss` order is absent in practice.
    let je_in_bss = count(
        &raw,
        "SELECT count(*) AS count FROM information_schema.tables \
         WHERE table_schema = 'bss' AND table_name = 'ledger_journal_entry'",
    )
    .await;
    assert_eq!(je_in_bss, 1, "ledger_journal_entry is created in bss");
    let je_in_public = count(
        &raw,
        "SELECT count(*) AS count FROM information_schema.tables \
         WHERE table_schema = 'public' AND table_name = 'ledger_journal_entry'",
    )
    .await;
    assert_eq!(
        je_in_public, 0,
        "ledger_journal_entry must NOT exist in public"
    );
}

/// Reproduces C1: with the original `bss,public` order the second boot through
/// the toolkit runner crashes. Kept as executable documentation that the fix
/// above is necessary (and that the regression test actually exercises the bug).
#[tokio::test]
#[ignore = "requires Docker (testcontainers); documents the C1 crash"]
async fn bss_first_search_path_crash_loops_on_second_boot() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();

    let db = connect_db(
        &url_with_search_path(port, "bss,public"),
        ConnectOpts::default(),
    )
    .await
    .expect("connect with bss,public search_path");

    // Boot 1 succeeds (bookkeeping lands in public, bss does not exist yet).
    let r1 = run_migrations_for_testing(&db, Migrator::migrations())
        .await
        .expect("boot 1 succeeds even with the buggy order");
    assert_eq!(r1.applied, 45);

    // Boot 2 finds bss first, creates a second empty bookkeeping table there,
    // reads an empty history, and re-runs `CREATE TABLE bss.ledger_journal_entry`,
    // which already exists -> error.
    let r2 = run_migrations_for_testing(&db, Migrator::migrations()).await;
    assert!(
        r2.is_err(),
        "second boot under bss,public must crash (C1); got {r2:?}"
    );
}

/// The payment-tables migration (`m20260622_000006`) creates the three counter
/// tables on `up` and drops them on `down`. Asserts each is queryable
/// (`SELECT count(*)` returns 0) after the full chain runs up, and absent (the
/// query errors) after the migration's `down` reverses it.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn payment_tables_created_on_up_and_dropped_on_down() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = Database::connect(&url).await.unwrap();

    // Run the whole chain up; the three payment tables must be queryable+empty.
    Migrator::up(&db, None).await.expect("up applies the chain");
    for table in [
        "ledger_payment_settlement",
        "ledger_payment_allocation",
        "ledger_payment_allocation_refund",
    ] {
        let n = count(&db, format!("SELECT count(*) AS count FROM bss.{table}")).await;
        assert_eq!(n, 0, "{table} is queryable and empty after up");
    }

    // Reverse every migration applied AFTER the payment-tables migration, then the
    // payment-tables (000006) migration itself; the three payment tables are gone.
    // `Migrator::down` reverts in REVERSE of the `migrations()` Vec order (not name
    // order): payment is at Vec position 6 of 45, so reverting down to-and-including
    // it is 45 − 6 + 1 = 40 steps. (Magic count — recompute when a migration is
    // added: it grew 9 → 24 → 28 → 29 → 31 → 34 → 35 → 36 → 38 → 39 → 40 over the VHP-1858
    // audit block + the S3 adjustment/note migrations + the Slice-5 FX substrate (incl.
    // the S5-F3 functional-currency column + the Slice-5 remediation cache-consistency and
    // snapshot-identity migrations) + the Slice-7 reconciliation/period-close substrate
    // + the exception-queue OPEN-uniq remediation index + the VHP-1853 posting-policy
    // table + the VHP-1843 verified-balance baseline + the C3 fx-revaluation-run marker
    // + the VHP-1986 fx-revaluation-mode table + the currency-scale immutability trigger,
    // all appended AFTER payment in the Vec.)
    Migrator::down(&db, Some(40))
        .await
        .expect("down reverses the last 40 migrations (to before payment)");
    for table in [
        "ledger_payment_settlement",
        "ledger_payment_allocation",
        "ledger_payment_allocation_refund",
    ] {
        let err = db
            .query_one(pg(format!("SELECT count(*) AS count FROM bss.{table}")))
            .await
            .expect_err("table must be absent after down");
        assert!(
            err.to_string().to_lowercase().contains("does not exist")
                || err.to_string().to_lowercase().contains(table),
            "unexpected error for dropped {table}: {err}"
        );
    }
}

/// The pending-event-queue migration (`m20260623_000008`) creates the durable
/// queue table on `up` and drops it on `down`. Asserts it is queryable
/// (`SELECT count(*)` returns 0) after the full chain runs up, and absent (the
/// query errors) after the migration's `down` reverses just that last step.
#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn pending_event_queue_created_on_up_and_dropped_on_down() {
    let container = Postgres::default().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let db = Database::connect(&url).await.unwrap();

    // Run the whole chain up; the queue table must be queryable and empty.
    Migrator::up(&db, None).await.expect("up applies the chain");
    let n = count(
        &db,
        "SELECT count(*) AS count FROM bss.ledger_pending_event_queue",
    )
    .await;
    assert_eq!(
        n, 0,
        "ledger_pending_event_queue is queryable and empty after up"
    );

    // Reverse every migration applied AFTER the queue migration, then the queue
    // (000008) migration itself. `Migrator::down` reverts in REVERSE of the
    // `migrations()` Vec order (not name order): the queue is at Vec position 8 of
    // 45, so reverting down to-and-including it is 45 − 8 + 1 = 38 steps. (Magic
    // count — recompute when a migration is added after the queue in the Vec: it
    // grew 7 → 22 → 26 → 27 → 29 → 32 → 33 → 36 → 37 → 38 over the VHP-1858 audit block + the
    // S3 adjustment/note migrations + the Slice-5 FX substrate (incl. the S5-F3
    // functional-currency column + the Slice-5 remediation cache-consistency and
    // snapshot-identity migrations) + the Slice-7 reconciliation/period-close substrate
    // + the exception-queue OPEN-uniq remediation index + the VHP-1853 posting-policy
    // table + the VHP-1843 verified-balance baseline + the C3 fx-revaluation-run marker
    // + the VHP-1986 fx-revaluation-mode table + the currency-scale immutability trigger.)
    Migrator::down(&db, Some(38))
        .await
        .expect("down reverses the last 38 migrations (to before the queue)");
    let err = db
        .query_one(pg(
            "SELECT count(*) AS count FROM bss.ledger_pending_event_queue",
        ))
        .await
        .expect_err("table must be absent after down");
    assert!(
        err.to_string().to_lowercase().contains("does not exist")
            || err
                .to_string()
                .to_lowercase()
                .contains("ledger_pending_event_queue"),
        "unexpected error for dropped ledger_pending_event_queue: {err}"
    );
}
