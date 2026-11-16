//! The gear's own database for a harness: which engine, and how it is opened.
//!
//! Every engine is opened through `toolkit_db::build_db` from the same
//! `database:` block shape a deployment configures, so the pool, the `SQLite`
//! pragmas and the `PostgreSQL` session settings are the production ones. The
//! harness then migrates it with the `Migrator` `EventBrokerModule::migrations()`
//! runs.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use sea_orm::ConnectionTrait;
use uuid::Uuid;

/// The engine and place the gear's metadata lives in. The events themselves
/// live in the storage backend, not here.
#[derive(Debug, Clone, Default)]
pub enum GearDb {
    /// A fresh temp file, opened the way the standalone configuration opens
    /// it: WAL, `busy_timeout` 5000, a pool of four. More than one connection
    /// is what lets two publishes race, as they do in production. The file
    /// and its `-wal` and `-shm` companions are removed, best effort, when
    /// their owner is dropped: the harness, the [`OpenedGearDb`], or the
    /// [`MigratedGearDb`]. On Unix the files are unlinked even while a
    /// connection still has them open; Windows refuses that, so there the
    /// files can stay in the temp dir.
    #[default]
    Sqlite,
    /// `schema` on the server at `url`, created by the harness and selected
    /// with `params.search_path`, as a deployment selects it.
    Postgres { url: String, schema: String },
    /// `database` on the server at `url` (whose own database part is ignored),
    /// created by the harness.
    Mysql { url: String, database: String },
}

/// An opened gear database. Keep it for as long as `db` (or a clone of it) is
/// in use: on Unix, dropping it removes a `SQLite` file even while a
/// connection still has it open.
pub struct OpenedGearDb {
    pub db: toolkit_db::Db,
    /// A DSN that reaches the same tables, for a test's own connection.
    pub dsn: String,
    /// The `SQLite` file this database lives in; `None` on a server engine.
    pub(crate) file: Option<SqliteFile>,
}

/// An opened and migrated gear database, as the harness runs on it. Keep it
/// bound for as long as its pool (or a clone of it) is in use: on Unix,
/// dropping it removes a `SQLite` file even while a connection still has it
/// open.
pub struct MigratedGearDb {
    pub(crate) db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    pub(crate) dsn: String,
    /// The `SQLite` file this database lives in; `None` on a server engine.
    pub(crate) file: Option<SqliteFile>,
}

impl MigratedGearDb {
    /// The pool over the migrated database.
    #[must_use]
    pub fn db(&self) -> &Arc<toolkit_db::DBProvider<toolkit_db::DbError>> {
        &self.db
    }
}

/// A `SQLite` gear database file, removed with its `-wal` and `-shm`
/// companions when dropped.
///
/// Removal is best effort and never panics, because it runs in a test's
/// teardown: a companion that does not exist is expected, and any other
/// error is ignored too. Connections may still be open at that point (a
/// harness's background tasks can hold the pool until the runtime stops): on
/// Linux and macOS the files are unlinked and each connection keeps its open
/// file until it closes, while Windows refuses to remove a file a connection
/// still holds, so it stays in the temp dir.
pub(crate) struct SqliteFile {
    path: PathBuf,
}

impl SqliteFile {
    /// A fresh `<prefix>-<uuid>.db` path in the temp dir. Nothing is created
    /// until a connection opens it.
    fn in_temp_dir(prefix: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("{prefix}-{}.db", Uuid::now_v7().simple()));
        Self { path }
    }

    /// The DSN that opens this file, creating it if it does not exist.
    fn dsn(&self) -> String {
        let mut file = self.path.to_string_lossy().replace('\\', "/");
        if !file.starts_with('/') {
            file.insert(0, '/');
        }
        format!("sqlite://{file}?mode=rwc")
    }
}

impl Drop for SqliteFile {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut name = OsString::from(&self.path);
            name.push(suffix);
            // Best effort, as the type's doc says.
            std::fs::remove_file(name).ok();
        }
    }
}

/// The pool the `SQLite` file is opened with: production-sized, so concurrent
/// requests really run on separate connections.
const SQLITE_POOL: u32 = 4;

impl GearDb {
    /// Creates the schema or database where the engine has one, and opens it
    /// unmigrated.
    pub async fn open(&self) -> OpenedGearDb {
        let (config, dsn, file) = match self {
            Self::Sqlite => {
                let file = SqliteFile::in_temp_dir("cf-eb-harness");
                (
                    serde_json::json!({
                        "engine": "sqlite",
                        "path": file.path,
                        "params": {
                            "journal_mode": "WAL",
                            "busy_timeout": "5000",
                            "synchronous": "NORMAL",
                        },
                        "pool": { "max_conns": SQLITE_POOL },
                    }),
                    file.dsn(),
                    Some(file),
                )
            }
            Self::Postgres { url, schema } => {
                let admin = sea_orm::Database::connect(url.as_str())
                    .await
                    .expect("connect to the postgres server");
                admin
                    .execute_unprepared(&format!("CREATE SCHEMA IF NOT EXISTS \"{schema}\""))
                    .await
                    .expect("create the gear schema");
                admin.close().await.expect("close the admin connection");
                let separator = if url.contains('?') { '&' } else { '?' };
                (
                    serde_json::json!({
                        "engine": "postgres",
                        "dsn": url,
                        "params": { "search_path": schema },
                    }),
                    format!("{url}{separator}options=-c%20search_path%3D{schema}"),
                    None,
                )
            }
            Self::Mysql { url, database } => {
                let admin = sea_orm::Database::connect(url.as_str())
                    .await
                    .expect("connect to the mysql server");
                admin
                    .execute_unprepared(&format!("CREATE DATABASE IF NOT EXISTS `{database}`"))
                    .await
                    .expect("create the gear database");
                admin.close().await.expect("close the admin connection");
                let base = url.rsplit_once('/').map_or(url.as_str(), |(base, _)| base);
                let dsn = format!("{base}/{database}");
                (
                    serde_json::json!({ "engine": "mysql", "dsn": dsn }),
                    dsn,
                    None,
                )
            }
        };
        let config: toolkit_db::DbConnConfig =
            serde_json::from_value(config).expect("a valid database block");
        let db = toolkit_db::build_db(config, None)
            .await
            .expect("open the gear database");
        OpenedGearDb { db, dsn, file }
    }

    /// [`Self::open`], then every migration this crate owns applied.
    pub(crate) async fn migrated(&self) -> MigratedGearDb {
        let OpenedGearDb { db, dsn, file } = self.open().await;
        toolkit_db::migration_runner::run_migrations_for_testing(
            &db,
            <crate::infra::storage::migrations::Migrator as sea_orm_migration::MigratorTrait>::migrations(),
        )
        .await
        .expect("migrations");
        MigratedGearDb {
            db: Arc::new(toolkit_db::DBProvider::new(db)),
            dsn,
            file,
        }
    }
}

/// A migrated `SQLite` database in a fresh temp file, on a pool of one
/// connection, for this crate's unit tests. Keep it bound for as long as its
/// pool (or a clone of it) is in use: dropping it removes the file, best
/// effort, as [`SqliteFile`] does.
#[cfg(test)]
pub(crate) struct ScratchSqlite {
    db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    dsn: String,
    /// Declared last, so it is dropped after this value's own pool handle.
    _file: SqliteFile,
}

#[cfg(test)]
impl ScratchSqlite {
    /// Opens `<prefix>-<uuid>.db` in the temp dir and applies every migration
    /// this crate owns.
    pub(crate) async fn open(prefix: &str) -> Self {
        let file = SqliteFile::in_temp_dir(prefix);
        let dsn = file.dsn();
        let opts = toolkit_db::ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..Default::default()
        };
        let db = toolkit_db::connect_db(&dsn, opts)
            .await
            .expect("connect sqlite");
        toolkit_db::migration_runner::run_migrations_for_testing(
            &db,
            <crate::infra::storage::migrations::Migrator as sea_orm_migration::MigratorTrait>::migrations(),
        )
        .await
        .expect("migrations");
        Self {
            db: Arc::new(toolkit_db::DBProvider::new(db)),
            dsn,
            _file: file,
        }
    }

    /// The pool over the database.
    pub(crate) fn db(&self) -> &Arc<toolkit_db::DBProvider<toolkit_db::DbError>> {
        &self.db
    }

    /// A DSN that reaches the same file, for a test's own connection.
    pub(crate) fn dsn(&self) -> &str {
        &self.dsn
    }
}

#[cfg(all(test, unix))]
#[path = "gear_db_tests.rs"]
mod tests;
