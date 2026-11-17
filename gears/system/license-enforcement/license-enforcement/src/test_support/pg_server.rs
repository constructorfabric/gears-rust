//! One `PostgreSQL` server for every test process, one `CREATE DATABASE` per
//! test.
//!
//! The shape of `gears/bss/pricing/pricing/tests/pg_support/mod.rs`; its module
//! doc gives the long form of each point:
//!
//! - `#[tokio::test]` builds one runtime per test, and a `ContainerAsync`
//!   dropped with it removes the container. The one this process starts is held
//!   by a dedicated thread with its own runtime, parked for the life of the
//!   process.
//! - A parked handle is never dropped, so the container outlives the process.
//!   It carries a fixed name, [`HARNESS_CONTAINER`], and is reused: a process
//!   that finds it running and answering connects to it. The leak is bounded at
//!   one container; `docker rm -fv license-enforcement-pg-harness` starts clean.
//! - Under nextest every test is a process, so losing the race for the name is
//!   the normal case: a loser waits for the winner's server to answer
//!   ([`BOOT_BUDGET`]), and only a container that the daemon reports as not
//!   running is force-removed.
//! - Databases of finished runs are dropped at start; a database is stale when
//!   the process in its name is gone from this host, never by connection.
//! - Every Docker question goes through the client that testcontainers starts
//!   containers with: some CI images have no `docker` CLI.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sea_orm::{ConnectOptions, ConnectionTrait, Database, Statement};
use testcontainers::bollard::Docker;
use testcontainers::bollard::errors::Error as BollardError;
use testcontainers::bollard::models::NetworkSettings;
use testcontainers::bollard::query_parameters::{
    InspectContainerOptionsBuilder, RemoveContainerOptionsBuilder,
};
use testcontainers::core::client::docker_client_instance;
use testcontainers::core::ports::Ports;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;

/// The fixed name of the one container, so that a later run finds it.
const HARNESS_CONTAINER: &str = "license-enforcement-pg-harness";

/// How long a sibling's container gets to boot: a cold runner pulls the image
/// and initialises a cluster while other test processes wait.
const BOOT_BUDGET: Duration = Duration::from_secs(90);

static SERVER: OnceLock<u16> = OnceLock::new();
static NEXT_DATABASE: AtomicU32 = AtomicU32::new(0);

/// A fresh database on the shared server; its DSN.
pub async fn fresh_database() -> String {
    let port = server_port();
    // The time keeps the name free when a finished run with the same process
    // id left its databases on the reused server.
    let database = format!(
        "t_{}_{}_{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
        NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
    );
    let admin = Database::connect(small_pool(&url(port, "postgres")))
        .await
        .expect("connect to the maintenance database");
    admin
        .execute_unprepared(&format!("CREATE DATABASE {database}"))
        .await
        .unwrap_or_else(|e| panic!("create database {database}: {e}"));
    drop(admin);
    url(port, &database)
}

/// The mapped port of the one server.
fn server_port() -> u16 {
    *SERVER.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build the container runtime");
            runtime.block_on(async move {
                let (port, _held) = resolve_server().await;
                prune_stale_databases(port).await;
                tx.send(port).expect("report the mapped port");
                // Holds the container, when this process started it.
                std::future::pending::<()>().await;
            });
        });
        rx.recv()
            .expect("the container thread must report its port")
    })
}

/// The server already running, or a fresh one under the same name. `Some`
/// container only when this process started it: a handle to a sibling's
/// container would remove it on drop.
async fn resolve_server() -> (u16, Option<ContainerAsync<Postgres>>) {
    let docker = daemon().await;
    if let Named::Running(Some(port)) = inspect_named(&docker).await
        && answers(port).await
    {
        return (port, None);
    }
    // A corpse of a killed run, a sibling still booting, or a daemon that
    // cannot tell: only the first licenses a removal.
    match await_answer(&docker).await {
        Awaited::Answered(port) => return (port, None),
        Awaited::Corpse => remove_named(&docker).await,
        Awaited::Undecided => panic!(
            "the container {HARNESS_CONTAINER} neither answered within {BOOT_BUDGET:?} nor \
             is stopped; if it is wedged, remove it with `docker rm -fv {HARNESS_CONTAINER}`"
        ),
    }
    if let Some(container) = start_named(&docker).await {
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("map the postgres port");
        return (port, Some(container));
    }
    let Awaited::Answered(port) = await_answer(&docker).await else {
        panic!("the sibling that won the name {HARNESS_CONTAINER} must bring it up")
    };
    (port, None)
}

async fn daemon() -> Docker {
    docker_client_instance()
        .await
        .unwrap_or_else(|e| panic!("configure the docker client of testcontainers: {e}"))
}

/// What the daemon says about the named container. `Unknown` licenses
/// nothing: it is a fact about the daemon, not about the container.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Named {
    /// Running, with the host port of 5432 once published.
    Running(Option<u16>),
    /// No such container, or one that is not running.
    Corpse,
    Unknown,
}

/// Whether the container runs and its port, from one inspect.
async fn inspect_named(docker: &Docker) -> Named {
    let options = InspectContainerOptionsBuilder::new().size(false).build();
    let info = match docker
        .inspect_container(HARNESS_CONTAINER, Some(options))
        .await
    {
        Ok(info) => info,
        Err(BollardError::DockerResponseServerError {
            status_code: 404, ..
        }) => return Named::Corpse,
        Err(_) => return Named::Unknown,
    };
    if !info.state.and_then(|s| s.running).unwrap_or(false) {
        return Named::Corpse;
    }
    Named::Running(host_port(info.network_settings))
}

/// Force-removes the container with its anonymous volume: the leak is a disk.
async fn remove_named(docker: &Docker) {
    let options = RemoveContainerOptionsBuilder::new()
        .force(true)
        .v(true)
        .build();
    drop(
        docker
            .remove_container(HARNESS_CONTAINER, Some(options))
            .await,
    );
}

#[derive(Clone, Copy, Debug)]
enum Awaited {
    Answered(u16),
    /// The only outcome that licenses a force-remove.
    Corpse,
    /// The budget ran out on a sibling still booting or a silent daemon.
    Undecided,
}

/// Waits up to [`BOOT_BUDGET`] for the container to answer.
async fn await_answer(docker: &Docker) -> Awaited {
    let deadline = Instant::now() + BOOT_BUDGET;
    loop {
        let state = inspect_named(docker).await;
        if let Named::Running(Some(port)) = state
            && answers(port).await
        {
            return Awaited::Answered(port);
        }
        if state == Named::Corpse {
            return Awaited::Corpse;
        }
        if Instant::now() >= deadline {
            return Awaited::Undecided;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Starts the container under its name; `None` when a sibling started it.
async fn start_named(docker: &Docker) -> Option<ContainerAsync<Postgres>> {
    let deadline = Instant::now() + BOOT_BUDGET;
    loop {
        let last = match test_containers::postgres()
            .with_container_name(HARNESS_CONTAINER)
            .start()
            .await
        {
            Ok(container) => return Some(container),
            Err(e) => e.to_string(),
        };
        if matches!(await_answer(docker).await, Awaited::Answered(_)) {
            return None;
        }
        assert!(
            Instant::now() < deadline,
            "postgres never started under the name {HARNESS_CONTAINER}: {last}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The IPv4 host port of 5432: [`url`] dials `127.0.0.1`.
fn host_port(settings: Option<NetworkSettings>) -> Option<u16> {
    Ports::try_from(settings?.ports?)
        .ok()?
        .map_to_host_port_ipv4(5432_u16)
}

async fn answers(port: u16) -> bool {
    let Ok(conn) = Database::connect(small_pool(&url(port, "postgres"))).await else {
        return false;
    };
    conn.execute_unprepared("SELECT 1").await.is_ok()
}

/// The process id in a database name that this module minted.
fn owning_pid(name: &str) -> Option<u32> {
    let (pid, rest) = name.strip_prefix("t_")?.split_once('_')?;
    rest.split('_')
        .all(|part| part.parse::<u128>().is_ok())
        .then(|| pid.parse().ok())?
}

/// Whether a process with this id runs on this host; `None` when `ps` cannot
/// be asked.
fn process_is_running(pid: u32) -> Option<bool> {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .ok()?;
    Some(out.stdout.iter().any(u8::is_ascii_digit))
}

/// Drops the databases of finished runs. `ps` is first asked about this very
/// process: a `ps` that cannot see it (another pid namespace) drops nothing.
/// Never `WITH (FORCE)`: a database still in use stays.
async fn prune_stale_databases(port: u16) {
    if process_is_running(std::process::id()) != Some(true) {
        return;
    }
    let Ok(conn) = Database::connect(small_pool(&url(port, "postgres"))).await else {
        return;
    };
    let Ok(rows) = conn
        .query_all_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            r"SELECT datname AS n FROM pg_database WHERE datname LIKE 't\_%'".to_owned(),
        ))
        .await
    else {
        return;
    };
    for row in rows {
        let Ok(name) = row.try_get::<String>("", "n") else {
            continue;
        };
        if owning_pid(&name).is_some_and(|pid| process_is_running(pid) == Some(false)) {
            drop(
                conn.execute_unprepared(&format!("DROP DATABASE {name}"))
                    .await,
            );
        }
    }
}

fn url(port: u16, database: &str) -> String {
    format!("postgres://postgres:postgres@127.0.0.1:{port}/{database}")
}

fn small_pool(url: &str) -> ConnectOptions {
    let mut options = ConnectOptions::new(url.to_owned());
    options.max_connections(2).min_connections(0);
    options
}
