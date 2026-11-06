//! The `PostgreSQL` container the `pg_*` suites share: brought up through
//! `testcontainers`, so each suite runs the chain on the backend a stand runs
//! on. Docker required: without it a suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it.

use sea_orm::{Database, DatabaseConnection};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use toolkit_db::{ConnectOpts, Db, connect_db};

/// A `PostgreSQL` container, the toolkit handle the runner takes, and a plain
/// connection for the rows the test writes and reads around the chain. Owned
/// by the test, so the container goes when the test does.
pub struct PgFixture {
    _container: ContainerAsync<Postgres>,
    pub db: Db,
    pub conn: DatabaseConnection,
}

/// Whether a missing or broken Docker must fail the run rather than skip it.
/// CI sets `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1`; locally it is unset.
fn require_docker() -> bool {
    std::env::var_os("SETTINGS_SERVICE_PG_REQUIRE_DOCKER")
        .is_some_and(|v| v != "0" && !v.is_empty())
}

/// Docker is not there: skip locally, fail where a skip would pass vacuously.
fn skip<T>(suite: &str, why: &str) -> Option<T> {
    assert!(!require_docker(), "PostgreSQL {suite} suite: {why}");
    eprintln!("skipping -- PostgreSQL {suite} suite: {why}");
    None
}

/// Bring up a `testcontainers` `PostgreSQL` and connect to it twice: once as
/// the toolkit does, for the runner, and once plainly, for the rows. `suite`
/// names the caller in the skip message.
pub async fn pg_fixture(suite: &str) -> Option<PgFixture> {
    let request = test_containers::postgres()
        .with_env_var("POSTGRES_PASSWORD", "pass")
        .with_env_var("POSTGRES_USER", "user")
        .with_env_var("POSTGRES_DB", "settings");
    let container = match request.start().await {
        Ok(container) => container,
        Err(e) => {
            return skip(
                suite,
                &format!(
                    "could not start a PostgreSQL container via testcontainers ({e}); install \
                     or start Docker to run this for real"
                ),
            );
        }
    };
    let port = match container.get_host_port_ipv4(5432).await {
        Ok(port) => port,
        Err(e) => {
            return skip(
                suite,
                &format!(
                    "the container started but its port could not be resolved ({e}); is Docker \
                     healthy?"
                ),
            );
        }
    };

    let url = format!("postgres://user:pass@127.0.0.1:{port}/settings");
    let opts = ConnectOpts {
        max_conns: Some(5),
        min_conns: Some(1),
        ..Default::default()
    };
    let db = connect_db(&url, opts)
        .await
        .expect("the toolkit connects to the container");
    let conn = Database::connect(&url)
        .await
        .expect("a plain connection to the container");
    Some(PgFixture {
        _container: container,
        db,
        conn,
    })
}
