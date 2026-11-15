//! The `SQLite` gear database file goes with its owner: once the harness, the
//! opened database or the migrated database is dropped, neither the file nor
//! its `-wal` and `-shm` companions is left in the temp dir. Unix only:
//! Windows refuses to remove a file a connection still holds open.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use sea_orm_migration::MigratorTrait;

use super::GearDb;
use crate::infra::storage::migrations::Migrator;
use crate::test_support::EventBrokerHarness;
use crate::test_support::module_boot::migrated_gear_db;

/// The database file a `SQLite` DSN (`sqlite://<path>?<params>`) names, with
/// its `-wal` and `-shm` companions.
fn database_files(dsn: &str) -> [PathBuf; 3] {
    let rest = dsn.strip_prefix("sqlite://").expect("a SQLite DSN");
    let path = Path::new(rest.split_once('?').map_or(rest, |(path, _)| path));
    let companion = |suffix: &str| {
        let mut name = OsString::from(path);
        name.push(suffix);
        PathBuf::from(name)
    };
    [path.to_owned(), companion("-wal"), companion("-shm")]
}

#[tokio::test]
async fn a_dropped_harness_leaves_no_database_file() {
    let harness = EventBrokerHarness::builder().build().await;
    let files = database_files(harness.dsn());
    // The migrations wrote through a pool that is still open, so the WAL and
    // its index exist beside the main file.
    assert_eq!(
        files.each_ref().map(|file| file.exists()),
        [true, true, true],
        "{files:?}"
    );

    drop(harness);

    assert_eq!(
        files.each_ref().map(|file| file.exists()),
        [false, false, false],
        "{files:?}"
    );
}

#[tokio::test]
async fn a_dropped_opened_database_leaves_no_file() {
    let opened = GearDb::Sqlite.open().await;
    let files = database_files(&opened.dsn);
    // Migrating writes through the pool, which stays open, so the WAL and its
    // index exist beside the main file.
    toolkit_db::migration_runner::run_migrations_for_testing(&opened.db, Migrator::migrations())
        .await
        .expect("migrations");
    assert_eq!(
        files.each_ref().map(|file| file.exists()),
        [true, true, true],
        "{files:?}"
    );

    drop(opened);

    assert_eq!(
        files.each_ref().map(|file| file.exists()),
        [false, false, false],
        "{files:?}"
    );
}

#[tokio::test]
async fn a_dropped_migrated_database_leaves_no_file() {
    let migrated = migrated_gear_db(&GearDb::Sqlite).await;
    let files = database_files(&migrated.dsn);
    // The migrations wrote through the pool it holds, which is still open, so
    // the WAL and its index exist beside the main file.
    assert_eq!(
        files.each_ref().map(|file| file.exists()),
        [true, true, true],
        "{files:?}"
    );

    drop(migrated);

    assert_eq!(
        files.each_ref().map(|file| file.exists()),
        [false, false, false],
        "{files:?}"
    );
}
