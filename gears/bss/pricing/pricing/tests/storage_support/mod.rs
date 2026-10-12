//! File-backed `SQLite` for repository races.
#![allow(clippy::expect_used, clippy::unwrap_used)]
use toolkit::contracts::DatabaseCapability;
use toolkit_db::secure::AccessScope;
use toolkit_db::{ConnectOpts, DBProvider, DbError};
use uuid::Uuid;
/// A test database's DSN and the temporary directory that holds its file, with the file's `-wal`
/// and `-shm`. The directory is removed when the last clone drops, so a test binds this for its
/// whole life (`_dsn`, never `_`, which drops it at once). It reads as the DSN: `&dsn` is a `&str`.
#[derive(Clone, Debug)]
pub struct TestDsn {
    dsn: String,
    _dir: Option<std::sync::Arc<tempfile::TempDir>>,
}
impl TestDsn {
    /// A new, empty database file in a new directory of the user's temp dir named `prefix…`.
    ///
    /// # Panics
    /// Panics if the temporary directory cannot be created.
    #[must_use]
    pub fn new(prefix: &str) -> Self {
        let dir = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
        let dsn = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("db.sqlite3").display()
        );
        Self {
            dsn,
            _dir: Some(std::sync::Arc::new(dir)),
        }
    }
    /// A DSN no temporary directory backs: a Postgres test's database URL.
    #[allow(
        dead_code,
        reason = "only the binaries that build a fixture over Postgres call it"
    )]
    #[must_use]
    pub fn of(dsn: String) -> Self {
        Self { dsn, _dir: None }
    }
}
impl std::ops::Deref for TestDsn {
    type Target = str;
    fn deref(&self) -> &str {
        &self.dsn
    }
}
impl std::fmt::Display for TestDsn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.dsn)
    }
}
/// `Database::connect(&dsn)` takes anything that is `Into<String>`.
impl From<&TestDsn> for String {
    fn from(dsn: &TestDsn) -> Self {
        dsn.dsn.clone()
    }
}
/// A migrated file-backed database: provider, tenant scope, tenant and its [`TestDsn`], which the
/// caller holds for the test's life.
pub async fn test_db() -> (DBProvider<DbError>, AccessScope, Uuid, TestDsn) {
    let dsn = TestDsn::new("pricing-repos-");
    let db = toolkit_db::connect_db(
        &dsn,
        ConnectOpts {
            max_conns: Some(1),
            min_conns: Some(1),
            ..ConnectOpts::default()
        },
    )
    .await
    .unwrap();
    toolkit_db::migration_runner::run_migrations_for_testing(
        &db,
        bss_pricing::module::BssPricingGear::default().migrations(),
    )
    .await
    .unwrap();
    let tenant = Uuid::new_v4();
    (
        DBProvider::new(db),
        AccessScope::for_tenant(tenant),
        tenant,
        dsn,
    )
}
pub fn at(hour: u8) -> time::OffsetDateTime {
    time::Date::from_calendar_date(2026, time::Month::September, 26)
        .unwrap()
        .with_hms(hour, 0, 0)
        .unwrap()
        .assume_utc()
}
