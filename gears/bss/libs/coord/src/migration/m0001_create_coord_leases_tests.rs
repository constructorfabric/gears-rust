//! The migration is applied by **every** gear that coordinates singleton work,
//! and the toolkit keeps migration bookkeeping per gear — so on a shared
//! database the second gear to boot applies `m0001_…` against a table the first
//! one already created.
//!
//! No gear's own suite can catch that: each runs against a database only that
//! gear has migrated, so `up` is only ever seen on an empty schema. The failure
//! surfaces for the first time in a cluster, at boot, as a crash loop — which is
//! exactly how it surfaced when `bss-pricing` first booted alongside a
//! `bss-ledger` that had been running for weeks.
//!
//! These tests are therefore about the SQL rather than about the runner: they
//! call `up` directly, twice, which is the shape the two-gear case actually
//! takes. Going through `run_migrations_for_testing` twice would prove nothing —
//! its bookkeeping would skip the second run, which is precisely what does NOT
//! happen across two gears.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use sea_orm_migration::MigrationTrait;
use sea_orm_migration::SchemaManager;
use sea_orm_migration::sea_orm::{Database, DatabaseConnection};

use super::Migration;

/// A bare sea-orm connection rather than the toolkit `Db` the lease tests use.
///
/// The subject here is the migration's SQL, and `SchemaManager` is what applies
/// it; going through the toolkit would add a layer that has no bearing on
/// whether `CREATE TABLE` tolerates an existing table.
async fn fresh_conn() -> DatabaseConnection {
    Database::connect("sqlite::memory:")
        .await
        .expect("connect in-memory sqlite")
}

#[tokio::test]
async fn a_second_gear_applying_the_same_migration_does_not_fail() {
    // The regression. The first `up` stands in for the gear that booted first
    // (or booted weeks ago); the second for the gear whose own bookkeeping is
    // empty and which therefore applies `m0001_…` again.
    let conn = fresh_conn().await;
    let manager = SchemaManager::new(&conn);

    Migration::unqualified()
        .up(&manager)
        .await
        .expect("the first gear creates the table");

    Migration::unqualified().up(&manager).await.expect(
        "the second gear must tolerate a table the first one created -- without this, \
         it dies at boot on `relation \"coord_leases\" already exists` before serving anything",
    );
}

#[tokio::test]
async fn the_table_is_usable_after_a_repeated_apply() {
    // Idempotence that produced an unusable table would be worse than the
    // crash: the gear would boot and then fail on its first lease acquisition.
    // So the second `up` must leave the table the first one made, intact.
    let conn = fresh_conn().await;
    let manager = SchemaManager::new(&conn);

    Migration::unqualified().up(&manager).await.unwrap();
    Migration::unqualified().up(&manager).await.unwrap();

    assert!(
        manager
            .has_table("coord_leases")
            .await
            .expect("ask the schema"),
        "the repeated apply must leave the table in place"
    );
    for column in ["key", "locked_by", "locked_until", "attempts"] {
        assert!(
            manager
                .has_column("coord_leases", column)
                .await
                .expect("ask the schema"),
            "the repeated apply must not have altered the table: {column} is missing"
        );
    }
}

#[tokio::test]
async fn down_then_up_still_works() {
    // `down` was already idempotent (`DROP TABLE IF EXISTS`). Pinning the round
    // trip keeps the pair honest: a future edit that made `up` conditional on
    // some state `down` does not restore would break here rather than in a
    // cluster.
    let conn = fresh_conn().await;
    let manager = SchemaManager::new(&conn);

    Migration::unqualified().up(&manager).await.unwrap();
    Migration::unqualified().down(&manager).await.unwrap();
    assert!(!manager.has_table("coord_leases").await.unwrap());

    Migration::unqualified().up(&manager).await.unwrap();
    assert!(manager.has_table("coord_leases").await.unwrap());
}
