//! What the `PostgreSQL` suites share: the container, its URL, and the one
//! rule for a missing Docker. Each suite wraps this in a fixture of its own —
//! a container per test stays, as every test wants a database nobody else
//! touched — and adds the connections and the schema it needs.
//!
//! Docker required: without it a suite skips, unless
//! `SETTINGS_SERVICE_PG_REQUIRE_DOCKER=1` turns the skip into a failure, as CI
//! sets it.

/// The macro that writes the refusing half of a scripted broker, shared with
/// the lib's unit tests.
#[path = "../../src/test_support/event_broker_stubs.rs"]
#[allow(unused_imports)]
pub mod event_broker_stubs;
pub mod instances;
pub mod platform;

use testcontainers::ContainerAsync;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

/// Whether a missing or broken Docker must fail the run rather than skip it.
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

/// A running container and the URL of its `settings` database.
pub struct Container {
    /// Held so the container goes when the test does.
    pub _container: ContainerAsync<Postgres>,
    /// `postgres://…` of the database.
    pub url: String,
}

/// Start a `PostgreSQL` container for the named suite, or `None` (a skip) when
/// Docker cannot provide one.
pub async fn start(suite: &str) -> Option<Container> {
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
                    "the container started but its port could not be resolved ({e}); is \
                     Docker healthy?"
                ),
            );
        }
    };
    Some(Container {
        _container: container,
        url: format!("postgres://user:pass@127.0.0.1:{port}/settings"),
    })
}
