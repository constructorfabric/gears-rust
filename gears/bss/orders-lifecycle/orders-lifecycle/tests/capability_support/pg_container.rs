//! The one Orders PostgreSQL test-container start, shared by every Orders harness (integration
//! tests via `capability_support`, crate tests via `#[path]`).
//!
//! Docker Desktop under parallel load shows three start-up faults, all observed in this suite:
//! `start()` returns before 5432/tcp is published (transient), a running container never gets its
//! port published, and a published host port briefly answers with another service (an HTTP reply
//! to the PostgreSQL `SSLRequest`). So the helper retries the port lookup with bounded backoff,
//! then waits for a real PostgreSQL handshake (a stronger form of
//! `libs/toolkit-db/tests/common.rs::wait_for_tcp`, since TCP accept alone proved insufficient).
//! A container that fails either window is removed and a fresh one started, a bounded number of
//! times; the final error names every attempt.
use sea_orm::{ConnectOptions, ConnectionTrait, Database};
use std::time::Duration;
use testcontainers_modules::{
    postgres::Postgres,
    testcontainers::{ContainerAsync, runners::AsyncRunner},
};
use tokio::time::{Instant, sleep};

/// Per-container window for Docker to publish 5432/tcp.
const PUBLISH_WINDOW: Duration = Duration::from_secs(20);
/// Per-container window for PostgreSQL to complete a handshake on the published port.
const READY_WINDOW: Duration = Duration::from_secs(20);
/// Fresh containers started when one never becomes usable.
const START_ATTEMPTS: u32 = 3;

/// Start the repository's pinned PostgreSQL image; return it with its published, ready port.
///
/// # Errors
/// Container start failure, or no usable container after every attempt.
pub async fn start_postgres() -> anyhow::Result<(ContainerAsync<Postgres>, u16)> {
    let mut failures = Vec::new();
    for _ in 0..START_ATTEMPTS {
        let container = test_containers::postgres().start().await?;
        let ready = match published_port(&container).await {
            Ok(port) => wait_for_postgres(port).await.map(|()| port),
            Err(error) => Err(error),
        };
        match ready {
            Ok(port) => return Ok((container, port)),
            Err(error) => {
                let running = container.is_running().await.ok();
                failures.push(format!(
                    "container {} (running: {running:?}): {error}",
                    container.id()
                ));
                container.rm().await.ok();
            }
        }
    }
    anyhow::bail!(
        "no usable PostgreSQL container in {START_ATTEMPTS} attempts \
         (publish {PUBLISH_WINDOW:?}, ready {READY_WINDOW:?} each): {}",
        failures.join("; ")
    )
}

async fn published_port(container: &ContainerAsync<Postgres>) -> anyhow::Result<u16> {
    let deadline = Instant::now() + PUBLISH_WINDOW;
    let mut backoff = Duration::from_millis(50);
    loop {
        match container.get_host_port_ipv4(5432).await {
            Ok(port) => return Ok(port),
            Err(error) if Instant::now() >= deadline => return Err(error.into()),
            Err(_) => {
                sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(1));
            }
        }
    }
}

async fn wait_for_postgres(port: u16) -> anyhow::Result<()> {
    let deadline = Instant::now() + READY_WINDOW;
    let mut options = ConnectOptions::new(format!(
        "postgres://postgres:postgres@127.0.0.1:{port}/postgres"
    ));
    options
        .max_connections(1)
        .connect_timeout(Duration::from_secs(2))
        .sqlx_logging(false);
    loop {
        let attempt = async {
            let db = Database::connect(options.clone()).await?;
            db.execute_unprepared("SELECT 1").await?;
            db.close().await
        }
        .await;
        match attempt {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => {
                anyhow::bail!("127.0.0.1:{port} did not complete a PostgreSQL handshake: {error}")
            }
            Err(_) => sleep(Duration::from_millis(200)).await,
        }
    }
}
