//! A file `SQLite` database for the schema-guard suites: pricing's, and Products' through
//! `#[path]`. The gear's migration list runs through the toolkit runner (history table, name
//! sort, pending-only) on its own pool, and a second, raw connection can shape the file first.
#![allow(dead_code)]
#![allow(clippy::expect_used, clippy::unwrap_used)]
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};
use sea_orm_migration::MigrationTrait;
use toolkit_db::migration_runner::{MigrationError, MigrationResult, run_migrations_for_testing};
use toolkit_db::{ConnectOpts, connect_db};

/// A file database, so the runner's pool and a raw connection see the same schema.
pub struct Lite {
    /// The database's own temporary directory, removed with the `Lite` (the file, its `-wal` and
    /// its `-shm`).
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    /// The gear's whole migration list.
    migrations: fn() -> Vec<Box<dyn MigrationTrait>>,
}

impl Lite {
    /// An empty file database in a new temporary directory named `prefix…`, migrated with
    /// `migrations`.
    pub fn new(prefix: &str, migrations: fn() -> Vec<Box<dyn MigrationTrait>>) -> Self {
        let dir = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        let path = dir.path().join("db.sqlite3");
        Self {
            _dir: dir,
            path,
            migrations,
        }
    }

    fn dsn(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }

    /// The gear's whole migration list through the toolkit runner, on its own pool.
    pub async fn migrate(&self) -> Result<MigrationResult, MigrationError> {
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
        let result = run_migrations_for_testing(&db, (self.migrations)()).await;
        // The next connection to the file must not race this pool's close: a dropped pool
        // closes in the background, and on Windows its connection still locks the file.
        db.close().await.unwrap();
        result
    }

    /// A raw connection beside the runner's pool; close it before the file is reopened.
    pub async fn raw(&self) -> DatabaseConnection {
        Database::connect(self.dsn()).await.unwrap()
    }

    pub async fn exec(&self, statements: &[&str]) {
        let raw = self.raw().await;
        for sql in statements {
            raw.execute_raw(Statement::from_string(DbBackend::Sqlite, (*sql).to_owned()))
                .await
                .unwrap_or_else(|e| panic!("{sql}: {e}"));
        }
        raw.close().await.unwrap();
    }

    pub async fn tables(&self) -> Vec<String> {
        let raw = self.raw().await;
        let names = raw
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
             ORDER BY name"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .iter()
            .map(|row| row.try_get::<String>("", "name").unwrap())
            .collect();
        raw.close().await.unwrap();
        names
    }

    /// The tables whose names start with `prefix`: the gear's own.
    pub async fn tables_named(&self, prefix: &str) -> Vec<String> {
        self.tables()
            .await
            .into_iter()
            .filter(|t| t.starts_with(prefix))
            .collect()
    }

    /// The runner's history table (`run_migrations_for_testing` names it after `_test`).
    pub async fn history(&self) -> String {
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
    pub async fn forget(&self, names: &[&str]) {
        let ledger = self.history().await;
        for name in names {
            self.exec(&[&format!(
                r#"DELETE FROM "{ledger}" WHERE version = '{name}'"#
            )])
            .await;
        }
    }

    /// Record `names` as applied, as a database that ran them does.
    pub async fn remember(&self, names: &[&str]) {
        let ledger = self.history().await;
        for name in names {
            self.exec(&[&format!(
                r#"INSERT INTO "{ledger}" (version) VALUES ('{name}')"#
            )])
            .await;
        }
    }
}
