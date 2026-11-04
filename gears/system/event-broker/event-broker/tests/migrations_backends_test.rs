//! The gear's full `Migrator` (its own tables and the ingest outbox's) on every
//! engine the gear database may be: it applies to an empty database, a second
//! run under the same history applies nothing, and every migration's DDL runs
//! again over the existing tables without failing, which is what a deployment
//! whose history was lost, or a second history table, relies on.
//!
//! Needs a Docker daemon for `PostgreSQL` and `MySQL`:
//!
//! ```text
//! cargo test -p cf-gears-event-broker --features integration --test migrations_backends_test
//! ```

#![cfg(feature = "integration")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use event_broker::infra::storage::migrations::Migrator;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_gear;

use support::matrix::{Engine, Matrix};

/// In the order the runner applies them.
const EVERY_MIGRATION: [&str; 4] = [
    "m001_create_toolkit_outbox_schema__event_broker__ob_default",
    "m20260819_000001_spec_cache",
    "m20260819_000002_storage_facade",
    "m20260819_000003_backend_binding",
];

async fn migrations_apply_once_and_rerun_cleanly(engine: Engine) {
    let Some(matrix) = Matrix::start(engine, "migrations").await else {
        return;
    };
    let gear_db = matrix.gear_db().open().await.db;

    let first = run_migrations_for_gear(&gear_db, "event-broker", Migrator::migrations())
        .await
        .expect("the migrations apply to an empty database");
    assert_eq!(
        (first.applied, first.skipped, first.applied_names),
        (
            EVERY_MIGRATION.len(),
            0,
            EVERY_MIGRATION.map(str::to_owned).to_vec()
        )
    );

    let again = run_migrations_for_gear(&gear_db, "event-broker", Migrator::migrations())
        .await
        .expect("a second run under the same history succeeds");
    assert_eq!(
        (again.applied, again.skipped, again.applied_names),
        (0, EVERY_MIGRATION.len(), Vec::<String>::new())
    );

    // A history of its own: every migration's DDL runs again over the tables
    // the first run created.
    let rerun = run_migrations_for_gear(&gear_db, "event-broker-rerun", Migrator::migrations())
        .await
        .expect("every migration re-applies over existing tables");
    assert_eq!(
        (rerun.applied, rerun.skipped, rerun.applied_names),
        (
            EVERY_MIGRATION.len(),
            0,
            EVERY_MIGRATION.map(str::to_owned).to_vec()
        )
    );
}

#[tokio::test]
async fn migrations_apply_once_and_rerun_cleanly_on_sqlite() {
    migrations_apply_once_and_rerun_cleanly(Engine::Sqlite).await;
}

#[tokio::test]
async fn migrations_apply_once_and_rerun_cleanly_on_postgres() {
    migrations_apply_once_and_rerun_cleanly(Engine::Postgres).await;
}

#[tokio::test]
async fn migrations_apply_once_and_rerun_cleanly_on_mysql() {
    migrations_apply_once_and_rerun_cleanly(Engine::Mysql).await;
}
