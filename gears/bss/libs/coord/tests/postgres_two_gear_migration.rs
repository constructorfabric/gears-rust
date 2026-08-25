//! Postgres-only: the two-gear boot the `IF NOT EXISTS` fix was written for,
//! run against the dialect the crash happened on and in the shape both real
//! consumers use.
//!
//! `up` builds **two independent SQL literals**
//! (`m0001_create_coord_leases.rs`): a schema-qualified Postgres `CREATE TABLE`
//! and a bare `SQLite` one. The in-crate tests
//! (`m0001_create_coord_leases_tests.rs`) connect `sqlite::memory:`, so they
//! exercise the `SQLite` literal only — an edit that dropped `IF NOT EXISTS` from
//! the Postgres literal alone would reproduce the exact production crash loop
//! (`bss-pricing` booting beside a long-running `bss-ledger`) with a green
//! suite.
//!
//! Two things are Postgres-only here and neither is reachable from `SQLite`:
//!
//! * `Migration::in_schema("bss")` — the constructor **both** consumers pass
//!   (`gears/bss/ledger/ledger/src/infra/storage/migrations.rs`,
//!   `gears/bss/pricing/pricing/src/infra/storage/migrations.rs`). `SQLite` has a
//!   single namespace and ignores the schema entirely, so the `SQLite` tests can
//!   only ever call `unqualified()`.
//! * The `CREATE SCHEMA IF NOT EXISTS` that `up` issues before the table, whose
//!   own repeat-safety is likewise unasserted anywhere else.
//!
//! As with the `SQLite` twin, these call `up` **directly**, twice, rather than
//! going through `run_migrations_for_testing`: the runner's per-gear bookkeeping
//! would skip the second apply, which is precisely what does NOT happen across
//! two gears with separate bookkeeping.
//!
//! Ignored by default (Docker); run with
//! `cargo test -p cf-gears-bss-coord --test postgres_two_gear_migration -- --ignored`, or via
//! `make test-coord-pg`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use sea_orm_migration::MigrationTrait;
use sea_orm_migration::SchemaManager;
use sea_orm_migration::sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

use coord::migration::Migration;

use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::ContainerAsync;
use testcontainers_modules::testcontainers::runners::AsyncRunner;

/// A fresh Postgres, plus the container kept alive by the caller.
///
/// A bare sea-orm connection rather than the toolkit `Db` the lease tests use:
/// the subject is the migration's SQL and `SchemaManager` is what applies it.
async fn fresh_pg() -> (ContainerAsync<Postgres>, DatabaseConnection) {
    // Image pinned by cf-gears-test-containers, like every fixture in the repo.
    let container = cf_gears_test_containers::postgres().start().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    let conn = Database::connect(&url).await.expect("connect postgres");
    (container, conn)
}

/// Does the qualified table exist in the named schema — asked of the catalog
/// rather than of `SchemaManager::has_table`, which resolves through
/// `search_path` and would answer about `public.coord_leases` just as happily.
async fn table_in_schema(conn: &DatabaseConnection, schema: &str) -> bool {
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            format!(
                "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
                 WHERE table_schema = '{schema}' AND table_name = 'coord_leases') AS present"
            ),
        ))
        .await
        .expect("ask the catalog")
        .expect("EXISTS always returns a row");
    row.try_get::<bool>("", "present").expect("read the flag")
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn a_second_gear_applying_the_same_migration_does_not_fail_on_postgres() {
    // The regression, on the dialect it happened on. The first `up` stands in
    // for the gear that booted weeks ago; the second for the gear whose own
    // bookkeeping is empty and which therefore applies `m0001_…` again.
    let (_container, conn) = fresh_pg().await;
    let manager = SchemaManager::new(&conn);

    Migration::in_schema("bss")
        .up(&manager)
        .await
        .expect("the first gear creates the schema and the table");

    Migration::in_schema("bss").up(&manager).await.expect(
        "the second gear must tolerate a schema and a table the first one created -- without \
         `IF NOT EXISTS` on the PG literal it dies at boot on `relation \"bss.coord_leases\" \
         already exists` before serving anything",
    );

    assert!(
        table_in_schema(&conn, "bss").await,
        "the repeated apply must leave the qualified table in place"
    );
    assert!(
        !table_in_schema(&conn, "public").await,
        "the qualified `up` must not also create an unqualified twin in `public`: two tables \
         would split the lease and let two workers hold the same key"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn the_qualified_table_is_usable_after_a_repeated_apply() {
    // Idempotence that produced an unusable table would be worse than the
    // crash: the gear would boot and then fail on its first lease acquisition.
    let (_container, conn) = fresh_pg().await;
    let manager = SchemaManager::new(&conn);

    Migration::in_schema("bss").up(&manager).await.unwrap();
    Migration::in_schema("bss").up(&manager).await.unwrap();

    let columns = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT column_name FROM information_schema.columns \
             WHERE table_schema = 'bss' AND table_name = 'coord_leases'"
                .to_owned(),
        ))
        .await
        .expect("ask the catalog");
    let mut names: Vec<String> = columns
        .iter()
        .map(|r| r.try_get::<String>("", "column_name").expect("column name"))
        .collect();
    names.sort();

    assert_eq!(
        names,
        vec![
            "attempts".to_owned(),
            "key".to_owned(),
            "locked_by".to_owned(),
            "locked_until".to_owned()
        ],
        "the repeated apply must not have altered the table"
    );
}

#[tokio::test]
#[ignore = "requires Docker (testcontainers)"]
async fn down_then_up_still_works_on_postgres() {
    // `down` was already `DROP TABLE IF EXISTS`. Pinning the round trip on PG
    // keeps the pair honest against a future edit that made `up` conditional on
    // some state `down` does not restore — and asserts that `down` drops the
    // *qualified* table rather than a `search_path`-resolved one.
    let (_container, conn) = fresh_pg().await;
    let manager = SchemaManager::new(&conn);

    Migration::in_schema("bss").up(&manager).await.unwrap();
    Migration::in_schema("bss").down(&manager).await.unwrap();
    assert!(!table_in_schema(&conn, "bss").await);

    Migration::in_schema("bss").up(&manager).await.unwrap();
    assert!(table_in_schema(&conn, "bss").await);
}
