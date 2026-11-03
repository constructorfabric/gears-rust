//! The gear's own database for a harness: which engine, and how it is opened.
//!
//! Every engine is opened through `toolkit_db::build_db` from the same
//! `database:` block shape a deployment configures, so the pool, the `SQLite`
//! pragmas and the `PostgreSQL` session settings are the production ones. The
//! harness then migrates it with the `Migrator` `EventBrokerModule::migrations()`
//! runs.

use std::sync::Arc;

use sea_orm::ConnectionTrait;
use uuid::Uuid;

/// The engine and place the gear's metadata lives in. The events themselves
/// live in the storage backend, not here.
#[derive(Debug, Clone, Default)]
pub enum GearDb {
    /// A fresh temp file, opened the way the standalone configuration opens
    /// it: WAL, `busy_timeout` 5000, a pool of four. More than one connection
    /// is what lets two publishes race, as they do in production.
    #[default]
    Sqlite,
    /// `schema` on the server at `url`, created by the harness and selected
    /// with `params.search_path`, as a deployment selects it.
    Postgres { url: String, schema: String },
    /// `database` on the server at `url` (whose own database part is ignored),
    /// created by the harness.
    Mysql { url: String, database: String },
}

/// An opened gear database.
pub struct OpenedGearDb {
    pub db: toolkit_db::Db,
    /// A DSN that reaches the same tables, for a test's own connection.
    pub dsn: String,
}

/// An opened and migrated gear database, as the harness runs on it.
pub(crate) struct MigratedGearDb {
    pub(crate) db: Arc<toolkit_db::DBProvider<toolkit_db::DbError>>,
    pub(crate) dsn: String,
}

/// The pool the `SQLite` file is opened with: production-sized, so concurrent
/// requests really run on separate connections.
const SQLITE_POOL: u32 = 4;

impl GearDb {
    /// Creates the schema or database where the engine has one, and opens it
    /// unmigrated.
    pub async fn open(&self) -> OpenedGearDb {
        let (config, dsn) = match self {
            Self::Sqlite => {
                let mut path = std::env::temp_dir();
                path.push(format!("cf-eb-harness-{}.db", Uuid::now_v7().simple()));
                let mut file = path.to_string_lossy().replace('\\', "/");
                if !file.starts_with('/') {
                    file.insert(0, '/');
                }
                (
                    serde_json::json!({
                        "engine": "sqlite",
                        "path": path,
                        "params": {
                            "journal_mode": "WAL",
                            "busy_timeout": "5000",
                            "synchronous": "NORMAL",
                        },
                        "pool": { "max_conns": SQLITE_POOL },
                    }),
                    format!("sqlite://{file}?mode=rwc"),
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
                (serde_json::json!({ "engine": "mysql", "dsn": dsn }), dsn)
            }
        };
        let config: toolkit_db::DbConnConfig =
            serde_json::from_value(config).expect("a valid database block");
        let db = toolkit_db::build_db(config, None)
            .await
            .expect("open the gear database");
        OpenedGearDb { db, dsn }
    }

    /// [`Self::open`], then every migration this crate owns applied.
    pub(crate) async fn migrated(&self) -> MigratedGearDb {
        let OpenedGearDb { db, dsn } = self.open().await;
        toolkit_db::migration_runner::run_migrations_for_testing(
            &db,
            <crate::infra::storage::migrations::Migrator as sea_orm_migration::MigratorTrait>::migrations(),
        )
        .await
        .expect("migrations");
        MigratedGearDb {
            db: Arc::new(toolkit_db::DBProvider::new(db)),
            dsn,
        }
    }
}
