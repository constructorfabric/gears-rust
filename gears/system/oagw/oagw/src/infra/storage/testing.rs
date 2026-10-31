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

/// Connect to `dsn` recording its statements and apply the migrations,
/// whose statements are cleared from the recorder.
async fn recorded(dsn: &str) -> (Db, QueryRecorder) {
    let (db, recorder) = connect_with_recorder(dsn, opts(4))
        .await
        .expect("connect recorded test database");
    migrate(&db).await;
    recorder.clear();
    (db, recorder)
}

/// A fresh migrated `SQLite` database whose statements are recorded. The
/// migration statements are cleared from the recorder.
pub(crate) async fn recorded_sqlite() -> (Db, QueryRecorder) {
    recorded(&shared_sqlite_dsn()).await
}

/// A migrated database in a fresh `PostgreSQL` container (pinned in
/// `test-containers`). The container lives as long as the returned guard.
#[cfg(feature = "integration")]
pub(crate) async fn postgres() -> (impl std::any::Any, Db) {
    let (container, host, port) = start_postgres().await;
    let db = migrated(&format!("postgres://user:pass@{host}:{port}/oagw"), 4).await;
    (container, db)
}

/// [`postgres`], with the database's statements recorded (the migration
/// statements cleared).
#[cfg(feature = "integration")]
pub(crate) async fn recorded_postgres() -> (impl std::any::Any, Db, QueryRecorder) {
    let (container, host, port) = start_postgres().await;
    let (db, recorder) = recorded(&format!("postgres://user:pass@{host}:{port}/oagw")).await;
    (container, db, recorder)
}

/// A started `PostgreSQL` container accepting connections, with its host and port.
#[cfg(feature = "integration")]
async fn start_postgres() -> (impl std::any::Any, String, u16) {
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
    (container, host, port)
}

/// A migrated database in a fresh `MySQL` container (pinned in
/// `test-containers`). The container lives as long as the returned guard.
#[cfg(feature = "integration")]
pub(crate) async fn mysql() -> (impl std::any::Any, Db) {
    let (container, host, port) = start_mysql().await;
    let db = migrated(&format!("mysql://user:pass@{host}:{port}/oagw"), 4).await;
    (container, db)
}

/// [`mysql`], with the database's statements recorded (the migration
/// statements cleared).
#[cfg(feature = "integration")]
pub(crate) async fn recorded_mysql() -> (impl std::any::Any, Db, QueryRecorder) {
    let (container, host, port) = start_mysql().await;
    let (db, recorder) = recorded(&format!("mysql://user:pass@{host}:{port}/oagw")).await;
    (container, db, recorder)
}

/// A started `MySQL` container accepting connections, with its host and port.
#[cfg(feature = "integration")]
async fn start_mysql() -> (impl std::any::Any, String, u16) {
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
    (container, host, port)
}

/// A database server in a container, handing out fresh migrated databases:
/// for suites whose cases each need an empty database (the registry
/// reconcile reads every tenant's registry rows).
#[cfg(feature = "integration")]
pub(crate) struct DbServer {
    _container: Box<dyn std::any::Any>,
    /// DSN without a database, for an account that may create databases.
    base: String,
    /// DSN of the database the container created.
    admin: String,
}

#[cfg(feature = "integration")]
impl DbServer {
    /// A fresh, migrated database on this server.
    pub(crate) async fn fresh(&self) -> Db {
        use sea_orm::ConnectionTrait;

        let name = format!("oagw_{}", Uuid::new_v4().simple());
        let admin = sea_orm::Database::connect(&self.admin)
            .await
            .expect("connect to the server's database");
        admin
            .execute_unprepared(&format!("CREATE DATABASE {name}"))
            .await
            .expect("create a database");
        drop(admin);
        migrated(&format!("{}/{name}", self.base), 2).await
    }

    /// `InnoDB`'s count of deadlocks since the server started (`MySQL`).
    pub(crate) async fn mysql_deadlocks(&self) -> i64 {
        use sea_orm::{ConnectionTrait, Statement};

        let admin = sea_orm::Database::connect(&self.admin)
            .await
            .expect("connect to the server's database");
        let row = admin
            .query_one_raw(Statement::from_string(
                sea_orm::DbBackend::MySql,
                "SELECT `COUNT` AS deadlocks, `STATUS` AS status \
                 FROM information_schema.INNODB_METRICS WHERE `NAME` = 'lock_deadlocks'",
            ))
            .await
            .expect("read the deadlock counter")
            .expect("the deadlock counter exists");
        let status: String = row.try_get("", "status").expect("status");
        assert_eq!(status, "enabled", "the deadlock counter must be enabled");
        row.try_get("", "deadlocks").expect("deadlocks")
    }
}

/// A `PostgreSQL` server for [`DbServer::fresh`] databases.
#[cfg(feature = "integration")]
pub(crate) async fn postgres_server() -> DbServer {
    let (container, host, port) = start_postgres().await;
    let base = format!("postgres://user:pass@{host}:{port}");
    DbServer {
        _container: Box::new(container),
        admin: format!("{base}/oagw"),
        base,
    }
}

/// A `MySQL` server for [`DbServer::fresh`] databases (created as root).
#[cfg(feature = "integration")]
pub(crate) async fn mysql_server() -> DbServer {
    let (container, host, port) = start_mysql().await;
    let base = format!("mysql://root:root@{host}:{port}");
    DbServer {
        _container: Box::new(container),
        admin: format!("{base}/oagw"),
        base,
    }
}
