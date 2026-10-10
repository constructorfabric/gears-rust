//! Migrated test databases shared by the storage and gear tests: `SQLite`
//! always, `PostgreSQL` and `MySQL` behind the `integration` feature (Docker
//! required).

use std::path::PathBuf;

use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::test_support::{QueryRecorder, connect_with_recorder};
use toolkit_db::{ConnectOpts, Db, connect_db};
use uuid::Uuid;

use super::migrations::Migrator;

fn opts(max_conns: u32) -> ConnectOpts {
    ConnectOpts {
        max_conns: Some(max_conns),
        min_conns: Some(1),
        ..Default::default()
    }
}

/// Apply the migrations once, asserting they all ran.
async fn migrate(db: &Db) {
    let result = run_migrations_for_testing(db, Migrator::migrations())
        .await
        .expect("migrations apply");
    assert_eq!(result.applied, Migrator::migrations().len());
}

/// Connect and apply the migrations.
pub(crate) async fn migrated(dsn: &str, max_conns: u32) -> Db {
    let db = connect_db(dsn, opts(max_conns))
        .await
        .expect("connect test database");
    migrate(&db).await;
    db
}

/// The statements that migrating a fresh in-memory `SQLite` database issues.
pub(crate) async fn migration_statements() -> QueryRecorder {
    let (db, recorder) = connect_with_recorder("sqlite::memory:", opts(1))
        .await
        .expect("connect recorded test database");
    migrate(&db).await;
    recorder
}

/// A private, named in-memory `SQLite` database that every pooled connection
/// shares (`cache=shared`); the pool's minimum connection keeps it alive.
fn shared_sqlite_dsn() -> String {
    format!(
        "sqlite:file:oagw-{}?mode=memory&cache=shared",
        Uuid::new_v4()
    )
}

/// A fresh migrated `SQLite` database with a small pool.
pub(crate) async fn shared_sqlite() -> Db {
    migrated(&shared_sqlite_dsn(), 4).await
}

/// A database file in the system temp directory, removed with its `SQLite`
/// side files on drop. Two handles opened on it in turn behave like one
/// process before and after a restart.
pub(crate) struct SqliteFile(PathBuf);

impl SqliteFile {
    pub(crate) fn new() -> Self {
        Self(std::env::temp_dir().join(format!("oagw-{}.db", Uuid::new_v4())))
    }

    /// Connect to the file, creating it if missing, and apply the migrations.
    /// On an already migrated file they apply nothing, as on a restart.
    pub(crate) async fn open(&self) -> Db {
        let db = connect_db(&format!("sqlite://{}?mode=rwc", self.0.display()), opts(4))
            .await
            .expect("connect test database file");
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("migrations apply");
        db
    }
}

impl Drop for SqliteFile {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut path = self.0.clone().into_os_string();
            path.push(suffix);
            // Best effort: a leftover file in the temp directory is harmless.
            let _ = std::fs::remove_file(path);
        }
    }
}

/// A fresh migrated `SQLite` database whose statements are recorded. The
/// migration statements are cleared from the recorder.
pub(crate) async fn recorded_sqlite() -> (Db, QueryRecorder) {
    let (db, recorder) = connect_with_recorder(&shared_sqlite_dsn(), opts(4))
        .await
        .expect("connect recorded test database");
    migrate(&db).await;
    recorder.clear();
    (db, recorder)
}

/// A migrated database in a fresh `PostgreSQL` container (pinned in
/// `test-containers`). The container lives as long as the returned guard.
#[cfg(feature = "integration")]
pub(crate) async fn postgres() -> (impl std::any::Any, Db) {
    use std::time::Duration;

    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use tokio::net::TcpStream;
    use tokio::time::{Instant, sleep};

    let container = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "oagw")
        .start()
        .await
        .expect("start postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("postgres port");
    let host = container
        .get_host()
        .await
        .expect("postgres host")
        .to_string();

    let deadline = Instant::now() + Duration::from_mins(1);
    while TcpStream::connect((host.trim_matches(['[', ']']), port))
        .await
        .is_err()
    {
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {host}:{port}"
        );
        sleep(Duration::from_millis(200)).await;
    }

    let db = migrated(&format!("postgres://user:pass@{host}:{port}/oagw"), 4).await;
    (container, db)
}

/// A migrated database in a fresh `MySQL` container (pinned in
/// `test-containers`). The container lives as long as the returned guard.
#[cfg(feature = "integration")]
pub(crate) async fn mysql() -> (impl std::any::Any, Db) {
    use std::time::Duration;

    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use tokio::net::TcpStream;
    use tokio::time::{Instant, sleep};

    let container = test_containers::mysql()
        .with_env_var("MYSQL_ROOT_PASSWORD", "root")
        .with_env_var("MYSQL_USER", "user")
        .with_env_var("MYSQL_PASSWORD", "pass")
        .with_env_var("MYSQL_DATABASE", "oagw")
        .start()
        .await
        .expect("start mysql container");
    let port = container
        .get_host_port_ipv4(3306)
        .await
        .expect("mysql port");
    let host = container.get_host().await.expect("mysql host").to_string();

    let deadline = Instant::now() + Duration::from_mins(1);
    while TcpStream::connect((host.trim_matches(['[', ']']), port))
        .await
        .is_err()
    {
        assert!(
            Instant::now() < deadline,
            "timeout waiting for {host}:{port}"
        );
        sleep(Duration::from_millis(200)).await;
    }

    let db = migrated(&format!("mysql://user:pass@{host}:{port}/oagw"), 4).await;
    (container, db)
}
