//! The `PostgreSQL` and `MySQL` servers of the engine-matrix tests: a
//! container on `libs/test-containers`' pinned tag (`GEARS_TEST_PG_TAG` /
//! `GEARS_TEST_MYSQL_TAG` override it), or the server `EB_TEST_POSTGRES_URL` /
//! `EB_TEST_MYSQL_URL` names, the plugin lanes' own variables.
//!
//! Without Docker a test is skipped with the reason printed;
//! `EB_TEST_REQUIRE_DOCKER=1` (the `eb-gear-db` make lane, CI) makes that a
//! failure instead, so a broken daemon cannot pass the lane vacuously.

use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::mysql::Mysql;
use testcontainers_modules::postgres::Postgres;
use uuid::Uuid;

/// A server, kept alive for as long as this value lives.
pub struct Server {
    /// The server's admin url; its database part is one that always exists.
    pub url: String,
    _container: Container,
}

enum Container {
    External,
    Postgres(ContainerAsync<Postgres>),
    Mysql(ContainerAsync<Mysql>),
}

/// Whether a missing Docker fails the run rather than skipping the test.
fn require_docker() -> bool {
    std::env::var_os("EB_TEST_REQUIRE_DOCKER").is_some_and(|v| v != "0" && !v.is_empty())
}

/// `None` (after printing why) when the container cannot start and Docker is
/// not required; a panic when it is.
fn started<T, E: std::fmt::Display>(engine: &str, result: Result<T, E>) -> Option<T> {
    match result {
        Ok(container) => Some(container),
        Err(err) => {
            assert!(
                !require_docker(),
                "EB_TEST_REQUIRE_DOCKER=1 but the {engine} container did not start: {err}"
            );
            eprintln!("skipped: the {engine} container did not start (no Docker?): {err}");
            None
        }
    }
}

/// `purpose` names the container `eb-gear-<purpose>-<6 hex>`, so `docker ps`
/// shows which test started it.
fn container_name(purpose: &str) -> String {
    format!(
        "eb-gear-{purpose}-{}",
        &Uuid::new_v4().simple().to_string()[..6]
    )
}

pub async fn postgres(purpose: &str) -> Option<Server> {
    if let Ok(url) = std::env::var("EB_TEST_POSTGRES_URL") {
        return Some(Server {
            url,
            _container: Container::External,
        });
    }
    let container = started(
        "postgres",
        test_containers::postgres()
            // The gear's pool, the outbox's and the backend's own, per test.
            .with_cmd(["postgres", "-c", "max_connections=300"])
            .with_container_name(container_name(purpose))
            .start()
            .await,
    )?;
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("mapped port");
    Some(Server {
        url: format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres"),
        _container: Container::Postgres(container),
    })
}

pub async fn mysql(purpose: &str) -> Option<Server> {
    if let Ok(url) = std::env::var("EB_TEST_MYSQL_URL") {
        return Some(Server {
            url,
            _container: Container::External,
        });
    }
    let container = started(
        "mysql",
        test_containers::mysql()
            .with_cmd(["--max-connections=600"])
            .with_container_name(container_name(purpose))
            .start()
            .await,
    )?;
    let port = container
        .get_host_port_ipv4(3306)
        .await
        .expect("mapped port");
    Some(Server {
        url: format!("mysql://root@127.0.0.1:{port}/test"),
        _container: Container::Mysql(container),
    })
}

/// A schema or database name no other test uses.
pub fn fresh_name(role: &str) -> String {
    format!("evbk_{role}_{}", &Uuid::new_v4().simple().to_string()[..12])
}
