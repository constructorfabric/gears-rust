//! The database the suites run on (DESIGN §4 Testability): in-memory SQLite by
//! default, a `testcontainers` `PostgreSQL` with `--features integration`.
//! Without Docker the `PostgreSQL` path falls back to SQLite (general suites)
//! or skips (`pg_*` suites), unless `MODEL_REGISTRY_PG_REQUIRE_DOCKER=1` turns
//! that into a failure, as CI sets it.

// Each test binary uses its own subset of this module.
#![allow(dead_code)]

use model_registry::infra::storage::migrations::Migrator;
use sea_orm_migration::MigratorTrait;
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, Db, connect_db};

/// A migrated database and whatever keeps it alive. Hold `keep_alive` for as
/// long as `db` is in use.
pub struct TestDb {
    pub db: Db,
    pub keep_alive: KeepAlive,
}

/// Owns the `PostgreSQL` container, if any, so it goes when the harness does.
pub struct KeepAlive {
    #[cfg(feature = "integration")]
    _server: Option<PgServer>,
}

pub async fn connect(dsn: &str, max_conns: u32) -> Db {
    let opts = ConnectOpts {
        max_conns: Some(max_conns),
        min_conns: Some(1),
        ..Default::default()
    };
    connect_db(dsn, opts).await.expect("test database")
}

pub async fn migrate(db: &Db) {
    run_migrations_for_testing(db, Migrator::migrations())
        .await
        .expect("migrations");
}

/// A fresh, migrated database for one harness.
pub async fn test_db() -> TestDb {
    #[cfg(feature = "integration")]
    {
        if let Some(server) = pg_server("general").await {
            let db = connect(&server.url, 5).await;
            migrate(&db).await;
            return TestDb {
                db,
                keep_alive: KeepAlive {
                    _server: Some(server),
                },
            };
        }
    }
    // One connection: every connection to `sqlite::memory:` is its own database.
    let db = connect("sqlite::memory:", 1).await;
    migrate(&db).await;
    TestDb {
        db,
        keep_alive: KeepAlive {
            #[cfg(feature = "integration")]
            _server: None,
        },
    }
}

#[cfg(feature = "integration")]
pub use pg::{PgServer, pg_server};

#[cfg(feature = "integration")]
mod pg {
    use testcontainers::runners::AsyncRunner;
    use testcontainers::{ContainerAsync, ImageExt};
    use testcontainers_modules::postgres::Postgres;

    /// An empty `PostgreSQL` database in its own container.
    pub struct PgServer {
        _container: ContainerAsync<Postgres>,
        pub url: String,
    }

    fn require_docker() -> bool {
        std::env::var_os("MODEL_REGISTRY_PG_REQUIRE_DOCKER")
            .is_some_and(|v| v != "0" && !v.is_empty())
    }

    /// Docker is not there: `None` locally, a failure where CI requires it.
    fn unavailable<T>(suite: &str, why: &str) -> Option<T> {
        assert!(!require_docker(), "PostgreSQL {suite} suite: {why}");
        eprintln!("PostgreSQL {suite} suite not run on PostgreSQL: {why}");
        None
    }

    /// `suite` names the caller in the message.
    pub async fn pg_server(suite: &str) -> Option<PgServer> {
        let request = test_containers::postgres()
            .with_env_var("POSTGRES_PASSWORD", "pass")
            .with_env_var("POSTGRES_USER", "user")
            .with_env_var("POSTGRES_DB", "model_registry");
        let container = match request.start().await {
            Ok(container) => container,
            Err(e) => {
                return unavailable(
                    suite,
                    &format!("could not start a PostgreSQL container via testcontainers ({e})"),
                );
            }
        };
        let port = match container.get_host_port_ipv4(5432).await {
            Ok(port) => port,
            Err(e) => {
                return unavailable(
                    suite,
                    &format!("the container started but its port could not be resolved ({e})"),
                );
            }
        };
        Some(PgServer {
            _container: container,
            url: format!("postgres://user:pass@127.0.0.1:{port}/model_registry"),
        })
    }
}
