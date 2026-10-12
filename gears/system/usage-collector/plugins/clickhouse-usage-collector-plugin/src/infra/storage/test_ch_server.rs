//! One `ClickHouse` server per Docker **daemon**, one `CREATE DATABASE` per
//! test.
//!
//! Shared by both live tiers: the crate's own `#[cfg(test)]` modules reach it as
//! `crate::infra::storage::test_ch_server`, and `tests/common/mod.rs` compiles
//! this same file through `#[path]`. It therefore names no crate item — only
//! `testcontainers`, `clickhouse`, `tokio` and `test_containers`.
//!
//! # Why not a container per test
//!
//! `make test-usage-collector-ch` runs under nextest, which gives every *test*
//! its own process. A container per test was some seventy `clickhouse-server`
//! boots per run, and on a machine running several pipelines at once those
//! boots are what the suite spent its time on. A fresh **database** per test
//! keeps the isolation the suites rely on — every test still sees empty
//! `usage_records` / `usage_type_catalog` tables, so fixed tenant ids and
//! idempotency keys stay safe to reuse — for the cost of one `CREATE DATABASE`.
//!
//! # Named, reused, owned by a parked thread
//!
//! The shape is `gears/bss/pricing/pricing/tests/pg_support/mod.rs`; see its
//! module doc for the long form of each point.
//!
//! - `#[tokio::test]` builds one runtime per test, and a `ContainerAsync`
//!   dropped with it removes the container. The one this process starts is
//!   held by a dedicated thread with its own runtime, parked for the life of
//!   the process.
//! - A parked handle is never dropped, so the container outlives the process.
//!   It carries a fixed name ([`container_name`]) and is **reused**: a process
//!   that finds it running and answering connects to it. The leak is bounded
//!   at one container per pinned tag, and every process of every concurrent run
//!   on the same daemon shares it.
//! - Losing the race for the name is the normal case under nextest, so a loser
//!   waits for the winner's server to answer ([`BOOT_BUDGET`]) rather than
//!   racing it, and only a container the daemon reports as not running is ever
//!   force-removed. A daemon that cannot be asked licenses nothing.
//! - Every Docker question goes through [`docker_client_instance`], the client
//!   testcontainers itself starts containers with. No `docker` CLI, which some
//!   CI images do not have.
//!
//! # Where this departs from the pricing harness
//!
//! - **Errors are returned, not panicked**, so `bring_up_or_skip` keeps its
//!   skip / `CH_REQUIRE_DOCKER=1` contract.
//! - **The wait is async.** [`server_port`] awaits a `watch` channel rather
//!   than blocking on `recv()`, so a caller's `tokio::time::timeout` can
//!   actually cancel it.
//! - **Stale databases are pruned by age, not by owning pid.** Concurrent
//!   pipelines on one daemon may run in different pid namespaces, where `ps`
//!   would report a sibling pipeline's live pid as gone and its databases would
//!   be dropped under it. A database name carries its creation time instead
//!   (`t_<unix_secs>_<pid>_<n>`), and only one older than [`STALE_AFTER`] is
//!   dropped. Names this harness did not mint are never touched.
//!
//! # What a test must still do for itself
//!
//! Anything read from a server-wide system table must be narrowed to
//! `currentDatabase()`: `system.query_log` in particular now carries every
//! sibling test's statements.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use testcontainers::bollard::Docker;
use testcontainers::bollard::errors::Error as BollardError;
use testcontainers::bollard::models::NetworkSettings;
use testcontainers::bollard::query_parameters::{
    InspectContainerOptionsBuilder, RemoveContainerOptionsBuilder,
};
use testcontainers::core::WaitFor;
use testcontainers::core::client::docker_client_instance;
use testcontainers::core::ports::Ports;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ContainerRequest, GenericImage, ImageExt};
use tokio::sync::watch;

/// Password for the container's `default` user. Every `ClickHouse` fixture in
/// this repository spells the same value, `ClickHouseSidecar.DB_PASSWORD` in
/// `testing/e2e/lib/sidecars.py` included.
///
/// MUST be non-empty. The official image's entrypoint only opens `default` to
/// `::/0` when `CLICKHOUSE_USER` is non-default **or** `CLICKHOUSE_PASSWORD` is
/// non-empty; otherwise it restricts `default` to `127.0.0.1`/`::1`, which
/// rejects every connection arriving through the mapped host port.
pub const PASSWORD: &str = "ch_test_pw";

/// The server's HTTP interface inside the container.
const HTTP_PORT: u16 = 8123;

/// How long a sibling's container gets to start answering before this process
/// gives up on it. Only paid in full in the pathological case: every wait ends
/// the moment the server answers. It has to cover a cold pull plus first boot
/// on a loaded runner.
pub const BOOT_BUDGET: Duration = Duration::from_secs(90);

/// Ceiling on one probe or one admin statement, so a wedged (e.g. paused)
/// server costs a bounded wait rather than a hang.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Interval between probes.
const POLL: Duration = Duration::from_millis(250);

/// Age past which a per-test database counts as a finished run's leftover.
///
/// Far longer than any run, so a live run's database is never in reach.
pub const STALE_AFTER: Duration = Duration::from_hours(6);

/// What the parked thread publishes: `None` until it has resolved the server.
type Resolved = Option<Result<u16, String>>;

static SERVER: OnceLock<watch::Receiver<Resolved>> = OnceLock::new();

/// Names the per-test databases apart within one process.
static NEXT_DATABASE: AtomicU32 = AtomicU32::new(0);

/// The fixed name of the shared container for the pinned tag.
///
/// The tag is in the name so that two pipelines pinning different `ClickHouse`
/// versions on one daemon never share a server.
#[must_use]
pub fn container_name() -> String {
    let tag: String = test_containers::clickhouse_tag()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("uc-clickhouse-test-harness-{tag}")
}

/// The mapped HTTP port of the shared server, once it answers.
///
/// The first call in a process spawns the thread that resolves (and, when this
/// process starts it, holds) the container; every call then awaits its
/// result. Cancel-safe: wrap it in `tokio::time::timeout`.
///
/// # Errors
/// When Docker cannot be reached, the container cannot be started, or it never
/// answers within [`BOOT_BUDGET`].
pub async fn server_port() -> Result<u16, String> {
    let mut rx = SERVER.get_or_init(spawn_owner).clone();
    let resolved = rx
        .wait_for(Option::is_some)
        .await
        .map_err(|_closed| "the ClickHouse container thread exited without a result".to_owned())?;
    match &*resolved {
        Some(result) => result.clone(),
        None => Err("the ClickHouse container thread published no result".to_owned()),
    }
}

/// Create a fresh, empty database for one test on the shared server.
///
/// # Errors
/// When the `CREATE DATABASE` fails or does not finish within the probe
/// timeout.
pub async fn fresh_database(port: u16) -> Result<String, String> {
    let name = format!(
        "t_{}_{}_{}",
        unix_now(),
        std::process::id(),
        NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
    );
    let created = tokio::time::timeout(
        PROBE_TIMEOUT,
        admin_client(port)
            .query(&format!("CREATE DATABASE `{name}`"))
            .execute(),
    )
    .await;
    match created {
        Ok(Ok(())) => Ok(name),
        Ok(Err(e)) => Err(format!("create database {name}: {e}")),
        Err(_elapsed) => Err(format!(
            "create database {name}: no answer within {PROBE_TIMEOUT:?}"
        )),
    }
}

/// The creation time a per-test database name carries, when it is one
/// [`fresh_database`] minted (`t_<unix_secs>_<pid>_<n>`, nothing else).
#[must_use]
pub fn minted_at(name: &str) -> Option<u64> {
    let mut parts = name.strip_prefix("t_")?.split('_');
    let secs = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u32>().ok()?;
    parts.next()?.parse::<u32>().ok()?;
    parts.next().is_none().then_some(secs)
}

/// May this database be dropped — is it a leftover older than
/// [`STALE_AFTER`]? A name this harness did not mint is always kept.
#[must_use]
pub fn is_stale(name: &str, now_secs: u64) -> bool {
    minted_at(name).is_some_and(|at| now_secs.saturating_sub(at) > STALE_AFTER.as_secs())
}

/// A client on the server's default database, for probes and admin DDL.
fn admin_client(port: u16) -> clickhouse::Client {
    clickhouse::Client::default()
        .with_url(format!("http://127.0.0.1:{port}"))
        .with_user("default")
        .with_password(PASSWORD)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Spawn the thread that resolves the server and holds the container this
/// process starts, if any, for the life of the process.
fn spawn_owner() -> watch::Receiver<Resolved> {
    let (tx, rx) = watch::channel(None);
    // A failed spawn drops `tx`, which `server_port` reports as a closed
    // channel; there is nothing else to do with the error here.
    drop(
        std::thread::Builder::new()
            .name("ch-test-server".to_owned())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(e) => {
                        tx.send_replace(Some(Err(format!("build the container runtime: {e}"))));
                        return;
                    }
                };
                runtime.block_on(async move {
                    let (resolved, held) = match resolve_server().await {
                        Ok((port, held)) => (Ok(port), held),
                        Err(e) => (Err(e), None),
                    };
                    let port = resolved.as_ref().ok().copied();
                    tx.send_replace(Some(resolved));
                    // Off the critical path: callers already have the port.
                    if let Some(port) = port {
                        prune_stale_databases(port).await;
                    }
                    std::future::pending::<()>().await;
                    // Held, when this process started it, until the process
                    // exits; the line is never reached.
                    drop((tx, held));
                });
            }),
    );
    rx
}

/// The shared server: the one already running, or a fresh one under the same
/// name. The `Option` is the ownership — `Some` only when this process started
/// it, in which case the parked thread keeps it alive.
async fn resolve_server() -> Result<(u16, Option<ContainerAsync<GenericImage>>), String> {
    let docker = docker_client_instance().await.map_err(|e| {
        format!("configure the docker client testcontainers starts containers with: {e}")
    })?;
    // A daemon that is not there at all answers no inspect, which the reuse
    // logic must read as "cannot tell" and wait out for the whole
    // `BOOT_BUDGET`. Ask once up front, so a Docker-less run skips at once.
    match tokio::time::timeout(PROBE_TIMEOUT, docker.ping()).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return Err(format!("the docker daemon is unreachable: {e}")),
        Err(_elapsed) => {
            return Err(format!(
                "the docker daemon did not answer a ping within {PROBE_TIMEOUT:?}"
            ));
        }
    }
    let name = container_name();
    if let Named::Running(Some(port)) = inspect_named(&docker, &name).await
        && answers(port).await
    {
        return Ok((port, None));
    }
    // Not answering *yet*: a killed run's corpse, a sibling still booting, or
    // a daemon that could not be asked. Only the first licenses a removal, and
    // `await_answer` returns at once on it.
    match await_answer(&docker, &name).await {
        Awaited::Answered(port) => return Ok((port, None)),
        Awaited::Corpse => remove_named(&docker, &name).await,
        Awaited::Undecided => {
            return Err(format!(
                "the container named {name} neither answered within {BOOT_BUDGET:?} nor is \
                 a corpse, so it may be a sibling's and is not this process's to remove; if \
                 it is wedged, clear it by hand with `docker rm -fv {name}`"
            ));
        }
    }
    let container = match start_named(&docker, &name).await? {
        Started::Owned(container) => *container,
        // A sibling won the name and its server already answers.
        Started::Sibling(port) => return Ok((port, None)),
    };
    let port = container
        .get_host_port_ipv4(HTTP_PORT)
        .await
        .map_err(|e| format!("map the ClickHouse HTTP port: {e}"))?;
    // Publish only a server that answers, so no caller races its boot.
    let deadline = Instant::now() + BOOT_BUDGET;
    while !answers(port).await {
        if Instant::now() >= deadline {
            return Err(format!(
                "the ClickHouse container {name} never answered within {BOOT_BUDGET:?}"
            ));
        }
        tokio::time::sleep(POLL).await;
    }
    Ok((port, Some(container)))
}

/// The container request for the shared server.
///
/// Image and tag come from `test_containers`, never a local literal: `cargo
/// xtask check-test-container-pins` enforces it. `WaitFor::Nothing` because
/// this image logs to files under `/var/log/clickhouse-server`, so a log-based
/// wait can only time out; readiness is the `SELECT 1` probe.
///
/// No `CLICKHOUSE_DB`: setting it makes the entrypoint boot a temporary server
/// to create the database, stop it, and start the real one, so a probe can
/// answer and the server go away again a moment later. `default` exists anyway.
fn image() -> ContainerRequest<GenericImage> {
    test_containers::clickhouse()
        .with_wait_for(WaitFor::Nothing)
        .with_env_var("CLICKHOUSE_USER", "default")
        .with_env_var("CLICKHOUSE_PASSWORD", PASSWORD)
}

/// What the daemon says about the container carrying the harness's name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Named {
    /// Running, with the host port it publishes for 8123 — `None` while it has
    /// published none yet.
    Running(Option<u16>),
    /// No container of that name, or one that is not running. The only answer
    /// that licenses a force-remove.
    Corpse,
    /// The daemon did not answer. Licenses **nothing**.
    Unknown,
}

/// Ask the daemon about a container by name. Both halves of the reuse decision
/// — is it running, on which port — come out of one inspect.
async fn inspect_named(docker: &Docker, name: &str) -> Named {
    let inspected = docker
        .inspect_container(
            name,
            Some(InspectContainerOptionsBuilder::new().size(false).build()),
        )
        .await;
    let info = match inspected {
        Ok(info) => info,
        // A 404 *is* an answer: there is no container under that name.
        Err(BollardError::DockerResponseServerError {
            status_code: 404, ..
        }) => return Named::Corpse,
        Err(_) => return Named::Unknown,
    };
    if !info.state.and_then(|state| state.running).unwrap_or(false) {
        return Named::Corpse;
    }
    Named::Running(host_port(info.network_settings))
}

/// The host port a container publishes for 8123, IPv4 because the probes dial
/// `127.0.0.1`. Read from an inspect, because owning a `ContainerAsync` would
/// remove the container on drop.
fn host_port(settings: Option<NetworkSettings>) -> Option<u16> {
    Ports::try_from(settings?.ports?)
        .ok()?
        .map_to_host_port_ipv4(HTTP_PORT)
}

/// Force-remove the named container and its anonymous volumes. Only called on
/// a [`Named::Corpse`]; failure is nothing to act on, the `start` that follows
/// reports whether the name came free.
async fn remove_named(docker: &Docker, name: &str) {
    drop(
        docker
            .remove_container(
                name,
                Some(
                    RemoveContainerOptionsBuilder::new()
                        .force(true)
                        .v(true)
                        .build(),
                ),
            )
            .await,
    );
}

/// What a wait for the named container came to. Three outcomes, because "the
/// wait ended" and "there is nothing there" are different facts and only the
/// second licenses a force-remove.
#[derive(Clone, Copy, Debug)]
enum Awaited {
    /// The container published a port and answered on it.
    Answered(u16),
    /// The daemon says nothing is running under the name.
    Corpse,
    /// The budget ran out on a sibling still booting or a daemon that could
    /// not be asked. Licenses **nothing**.
    Undecided,
}

/// Wait up to [`BOOT_BUDGET`] for the named container to publish a port and
/// answer on it.
async fn await_answer(docker: &Docker, name: &str) -> Awaited {
    let deadline = Instant::now() + BOOT_BUDGET;
    loop {
        let state = inspect_named(docker, name).await;
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
        tokio::time::sleep(POLL).await;
    }
}

/// How [`start_named`] came by the server.
enum Started {
    /// This process started the container and must hold it. Boxed because a
    /// `ContainerAsync` dwarfs a port.
    Owned(Box<ContainerAsync<GenericImage>>),
    /// A sibling won the name; its server answered on this port.
    Sibling(u16),
}

/// Start the container under `name`; [`Started::Sibling`] with the port its
/// server answered on if a sibling started it first.
///
/// A name conflict is the expected outcome for every process but one, so
/// losing it hands control to [`await_answer`] instead of racing again. A
/// conflict with a corpse removes the corpse and retries, up to
/// [`BOOT_BUDGET`].
async fn start_named(docker: &Docker, name: &str) -> Result<Started, String> {
    let deadline = Instant::now() + BOOT_BUDGET;
    loop {
        let last = match image().with_container_name(name).start().await {
            Ok(container) => return Ok(Started::Owned(Box::new(container))),
            Err(e) => e.to_string(),
        };
        match await_answer(docker, name).await {
            Awaited::Answered(port) => return Ok(Started::Sibling(port)),
            Awaited::Corpse => remove_named(docker, name).await,
            Awaited::Undecided => {}
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "ClickHouse never started under the name {name}: {last}"
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Does a `ClickHouse` on this port accept the harness credentials and answer?
async fn answers(port: u16) -> bool {
    matches!(
        tokio::time::timeout(
            PROBE_TIMEOUT,
            admin_client(port).query("SELECT 1").fetch_one::<u8>(),
        )
        .await,
        Ok(Ok(1))
    )
}

/// Drop the per-test databases finished runs left on the shared server — any
/// [`fresh_database`] name older than [`STALE_AFTER`]. Every failure is
/// ignored: the cost of keeping one is a stale database, nothing more.
async fn prune_stale_databases(port: u16) {
    let admin = admin_client(port);
    let Ok(Ok(names)) = tokio::time::timeout(
        PROBE_TIMEOUT,
        admin
            .query("SELECT name FROM system.databases WHERE startsWith(name, 't_')")
            .fetch_all::<String>(),
    )
    .await
    else {
        return;
    };
    let now = unix_now();
    for name in names.iter().filter(|name| is_stale(name, now)) {
        // `is_stale` admits only `t_<digits>_<digits>_<digits>`, so the name
        // is safe to splice. `IF EXISTS` because every concurrent process
        // prunes the same list.
        drop(
            tokio::time::timeout(
                PROBE_TIMEOUT,
                admin
                    .query(&format!("DROP DATABASE IF EXISTS `{name}` SYNC"))
                    .execute(),
            )
            .await,
        );
    }
}
