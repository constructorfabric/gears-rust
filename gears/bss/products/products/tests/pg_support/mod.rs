//! One Postgres server per test **binary**, one `CREATE DATABASE` per test.
//!
//! # This harness is adopted, not invented
//!
//! It is a port of `gears/bss/pricing/pricing/tests/pg_support/mod.rs`, taken
//! deliberately rather than written fresh: that file is the record of a long
//! sequence of failures this gear would otherwise have to repeat, and every
//! design decision below was paid for there. **The measurements quoted in this
//! doc are the donor's**, cited so a reader can see what a decision rests on;
//! this gear has measured none of them for itself and does not claim to. What
//! changed in the port is named at the end.
//!
//! # Why this exists rather than a container per test
//!
//! The donor's Postgres suites were first written with
//! `Postgres::default().start()` inside every test. That does not scale and it
//! does not merely cost time: its Track P2 measured sporadic
//! `PortNotExposed { port: Tcp(5432) }` panics in whichever tests happened to be
//! starting at the same moment — the daemon reports a container up a beat before
//! its port binding is readable.
//!
//! Under a guard-by-removal discipline a *spurious* red is worse than a slow
//! run. The whole proof is "delete the guard, watch **exactly one** test fail",
//! and a flake is indistinguishable from the second test a removal was not
//! supposed to redden. So the harness itself has to be the least flaky thing in
//! the run.
//!
//! A fresh **database** per test buys the same isolation for a fraction of the
//! cost: every test still gets a virgin schema with no row any other test wrote,
//! so tests may keep reusing one handful of fixed ids.
//!
//! # Why the container is owned by a parked thread
//!
//! `#[tokio::test]` builds one runtime per test and tears it down when that test
//! returns, and a `ContainerAsync` dropped with it takes the server down. A
//! container held only by the *first* test's runtime would therefore be removed
//! the moment that test finished, and every later test would fail against a
//! server that had been deliberately killed.
//!
//! [`server_port`] hands it to a dedicated thread with its own current-thread
//! runtime, which parks on `std::future::pending()` for the life of the process.
//!
//! # The container is **named and reused**, because parking leaks it
//!
//! A parked thread is never joined and a `static` is never dropped, so nothing
//! removes the container when the process exits — and there is no reaper here to
//! do it either: this client removes on `Drop` and starts no Ryuk. Every run of
//! every Postgres binary would therefore leave a live Postgres behind.
//!
//! **That is not a tidiness point.** In one of the donor's working sessions the
//! leak filled the Docker VM's disk, and the next run answered with fifteen
//! `No space left on device` failures spread over one schema suite — fifteen
//! reds that said nothing about any schema. A harness whose own failures are
//! indistinguishable from the guard failures it exists to prove is worse than a
//! slow one.
//!
//! So the container carries a fixed name, [`HARNESS_CONTAINER`], and is
//! **reused**: a binary that finds it already running and answering connects to
//! it rather than starting another. The leak is bounded at exactly one
//! container, forever, instead of one per binary per run.
//!
//! Reuse means the databases accumulate too, so the harness drops the ones
//! previous runs left, at start ([`prune_stale_databases`]).
//!
//! # What decides that a database is stale is the **owning process**, not a
//! connection
//!
//! It used to be the connection: a plain `DROP DATABASE` and never
//! `WITH (FORCE)`, on the argument that one another process is connected to
//! refuses to drop, so a live run's databases are safe. **That argument is
//! false**, and it is written down here because it read as airtight. Nothing in
//! this harness holds a connection between operations: [`Pg`] is
//! `{port, database}` and owns no pool, and [`Pg::applied`] drops the pool it
//! built for the migration chain. Two concurrent `cargo test` invocations — the
//! case this file explicitly claims to hold — could therefore destroy each
//! other's databases mid-run, producing exactly the red that says nothing about
//! any guard that this file exists to eliminate.
//!
//! What replaces it is the process id already in the name. [`next_database`]
//! mints `t_<pid>_<n>`, so every database says which run made it, and
//! [`prunable`] drops one only when that process is **gone from this host** —
//! asked of `ps`, which is a fact about the run rather than about whether it
//! happens to be holding a socket this instant. This run's own databases are not
//! a special case and get no special code: this process is running, so they fail
//! the same test.
//!
//! Every failure mode of the liveness question lands on *keep*: an unparseable
//! name, a `ps` that is missing, a `ps` that answers in a shape this cannot read
//! (calibrated against this very process before anything is dropped), a pid
//! reused by an unrelated process. The cost of keeping is one stale database;
//! the cost of dropping wrongly is a corrupted concurrent run.
//!
//! `WITH (FORCE)` is still never used, now as a second line rather than as the
//! argument.
//!
//! # Contention for the container name is the normal case, not the exotic one
//!
//! The donor's note here began as "`cargo test` runs test *binaries*
//! sequentially, so this gear's suites do not normally contend for the shared
//! server". **The premise was true and the conclusion was not**, because a
//! `cargo nextest` run gives every *test* its own process: it is not binaries in
//! sequence contending never, it is as many processes as the runner has cores,
//! racing for one fixed container name, on every run.
//!
//! That mis-sizing cost the donor a red on 2026-08-19, the first time its tier
//! ever executed in CI: three of 380 failed at 3.03s each on Docker
//! `409 Conflict` for the container name, while the other 377 passed against the
//! container the winner had by then finished booting. The budget in
//! [`start_named`] was five attempts 500ms apart — about 2.5s of waiting — which
//! is less than a cold runner needs to pull the image and let Postgres accept a
//! first connection.
//!
//! What holds now: the reuse path still takes no lock, but a process that loses
//! the race **waits for the winner's container to answer** against a deadline
//! ([`BOOT_BUDGET`]) rather than a fixed attempt count, and the force-remove asks
//! the daemon whether the container is `running` before treating it as a corpse
//! — a booting sibling and a killed run's leftovers are indistinguishable
//! through "does it answer", and removing the former is how one lost race
//! becomes a cascade. The prune still skips every database whose run is alive.
//!
//! # The `docker` CLI has to reach the daemon testcontainers reaches, and
//! nothing here checks that
//!
//! [`published_port`] and the force-remove shell out to `docker`, while
//! [`start_named`] goes through the testcontainers client. Where the two resolve
//! to **different** daemons — a `DOCKER_HOST` the CLI reads and the client does
//! not, two contexts, a rootless daemon beside a system one — the first run
//! starts a container the CLI cannot see, and every later run finds no published
//! port, force-removes nothing, and fails to start under a name that is already
//! taken. It is reported rather than closed: the check would be a third way of
//! asking the same question, and the failure is loud and repeatable rather than
//! silent.
//!
//! # What a suite must still do for itself
//!
//! A `must_be_rejected` helper is deliberately **not** here. Every suite asserts
//! that a refusal is *the one under test* — a bare "some error happened" would
//! pass with the guard it means to prove switched off — and the fragment that
//! makes that assertion sharp differs per suite: a constraint name here, a
//! trigger's literal message there. Hoisting them into one helper means taking
//! the weakest of them, which is how a suite ends up green against a schema that
//! no longer holds.
//!
//! **Anything a test reads out of a server-wide catalog must be narrowed to
//! `current_database()`.** One server carries every test's database at once, so
//! `pg_locks`, `pg_stat_activity` and friends see other tests' backends.
//! [`blocked_backends`] is the live instance of this.
//!
//! # What the port changed
//!
//! Three things, and nothing else:
//!
//! 1. [`HARNESS_CONTAINER`] is this gear's own name. Sharing the donor's one
//!    container would make two gears' runs prune each other's databases and let
//!    either force-remove a container the other was using; the leak stays
//!    bounded at one container *per gear*, which is the same bound.
//! 2. The chain applied by [`Pg::applied`] is this gear's [`Migrator`].
//! 3. The donor's frozen-column census is **not** ported. It has no consumer
//!    here yet, and a census nothing calls is dead code that reads like
//!    coverage. It comes with the suite that needs it, or not at all.

#![allow(
    dead_code,
    reason = "each test binary compiles the whole module and uses part of it"
)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use bss_products::infra::storage::migrations::Migrator;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, Db, connect_db};

/// The image tag every Postgres suite of this gear pins.
///
/// Pinned rather than defaulted so the engine a guard was proved against is a
/// fact of the repository and not of whatever the image's moving tag resolved to
/// on the day. The same tag the donor pins. Below the workspace floor
/// (`test_containers::POSTGRES_TAG`) as pricing's is, and taken through
/// `test_containers::postgres_tagged` so `GEARS_TEST_PG_TAG` still overrides it
/// (docs/TESTING.md 4.4).
pub const PG_TAG: &str = "16-alpine";

/// The one container's name — fixed, so a later run finds it instead of
/// starting another. See the module doc for why reuse rather than cleanup, and
/// why this is **this gear's own** name rather than the donor's.
pub const HARNESS_CONTAINER: &str = "bss-products-pg-harness";

/// The mapped port of the one server, resolved on first use.
static SERVER: OnceLock<u16> = OnceLock::new();

/// Names the per-test databases apart. An atomic rather than a uuid, so a
/// failure message points at a database a human can go and look at; the process
/// id is in there too, because the server outlives the process.
static NEXT_DATABASE: AtomicU32 = AtomicU32::new(0);

/// The one server's mapped port; see the module doc for why the container is
/// owned by a parked thread, and why it is named and reused.
pub fn server_port() -> u16 {
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
                // Hold the container - when this process is the one that
                // started it - for the life of the process.
                std::future::pending::<()>().await;
            });
        });
        rx.recv()
            .expect("the container thread must report its port")
    })
}

/// The shared server: the one already running, or a fresh one under the same
/// name.
///
/// The `Option` **is** the ownership. `None` when the server was already there —
/// this process did not start it and must not hold a handle whose `Drop` would
/// remove it out from under a sibling — and `Some` when it did, in which case
/// the parked thread is what keeps it alive.
async fn resolve_server() -> (u16, Option<ContainerAsync<Postgres>>) {
    if let Some(port) = published_port(HARNESS_CONTAINER)
        && answers(port).await
    {
        return (port, None);
    }
    // Something under our name is not answering *yet*. **Three** cases, not two,
    // and conflating any of them is what turns one lost race into a cascade: a
    // container a killed run left behind, a sibling's container still coming up,
    // and a daemon that could not be asked at all. Ask which, rather than
    // inferring it from the silence.
    match container_verdict(HARNESS_CONTAINER) {
        // Up, or on its way up. Wait for it rather than fight it.
        Some(Verdict::Live) => {
            if let Some(port) = await_answer(HARNESS_CONTAINER).await {
                return (port, None);
            }
            // Live for the whole budget and never answered: a corpse after all.
            let _ = docker(&["rm", "-f", HARNESS_CONTAINER]);
        }
        // Gone, or dead beyond recovery. Clearing the name is safe - the name is
        // this harness's own.
        Some(Verdict::Corpse) => {
            let _ = docker(&["rm", "-f", HARNESS_CONTAINER]);
        }
        // **The daemon could not be asked. Destroy nothing.** Falling through to
        // `start_named` is the safe move: its name-conflict path waits for
        // whoever holds the name instead of removing them.
        None => {}
    }
    if let Some(container) = start_named().await {
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("map the postgres port");
        return (port, Some(container));
    }
    // A sibling binary won the race and started it under our name. It may still
    // be booting, so wait rather than read a port it has not published yet.
    //
    // **This is the second `await_answer` on one compound path, and it is not a
    // duplicate.** Reaching here from `Verdict::Live` means the first call
    // spent its whole budget on a container that was then `rm -f`'d as a
    // corpse; this call waits on a *different* container, the sibling's, which
    // did not exist when the first one gave up. Collapsing them would make the
    // harness read a port belonging to the thing it had just removed. The cost
    // is a second `BOOT_BUDGET` on the rarest path — Live, then silent for the
    // full budget, then out-raced — and that is the price of not destroying a
    // sibling's container.
    let port = await_answer(HARNESS_CONTAINER)
        .await
        .expect("the sibling's container must come up and publish a port");
    (port, None)
}

/// How long a sibling's container gets to finish booting Postgres before this
/// process gives up on it.
///
/// Generous on purpose, and only ever paid in the pathological case: the wait
/// ends the instant the server answers, and ends early if the container stops
/// running. What it has to cover is a cold CI runner pulling the image and
/// initialising a cluster while a dozen sibling processes watch.
const BOOT_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);

/// What the daemon says about a container under this name.
///
/// **`None` means the question could not be asked** — no `docker` on the path,
/// a socket that refused, a daemon under load — and it is a distinct value from
/// [`Verdict::Corpse`] for exactly the reason [`process_is_running`] returns an
/// `Option`: the module doc promises that every failure mode of a liveness
/// question lands on *keep*, and the only caller of this function destroys
/// things. An earlier revision returned a bare `bool` and merged the two, so a
/// single transient `docker inspect` failure force-removed a healthy container
/// out from under every sibling process — the precise "reds that said nothing
/// about any schema" cascade this file exists to eliminate.
///
/// `created` and `restarting` are **not** corpses. A container holds its name in
/// `created` for the moments between `docker create` and `docker start`, so a
/// sibling entering [`resolve_server`] in that window would otherwise read the
/// winner's brand-new container as dead and remove it.
pub fn container_verdict(name: &str) -> Option<Verdict> {
    let status = docker(&["inspect", "-f", "{{.State.Status}}", name])?;
    Some(match status.trim() {
        "running" | "created" | "restarting" => Verdict::Live,
        _ => Verdict::Corpse,
    })
}

/// The daemon's answer about the harness container, once the "could not ask"
/// case has been lifted into the surrounding `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Running, or on its way there. Wait for it; never remove it.
    Live,
    /// Absent, exited, dead or removing. The name may be cleared.
    Corpse,
}

/// Wait for the named container to publish a port and answer on it, up to
/// [`BOOT_BUDGET`]; `None` if it stopped running or the budget ran out.
async fn await_answer(name: &str) -> Option<u16> {
    let deadline = std::time::Instant::now() + BOOT_BUDGET;
    loop {
        if let Some(port) = published_port(name)
            && answers(port).await
        {
            return Some(port);
        }
        if std::time::Instant::now() >= deadline || container_verdict(name) == Some(Verdict::Corpse)
        {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Start the one container under [`HARNESS_CONTAINER`], retrying; `None` if a
/// sibling started it first.
///
/// Retried rather than tolerated in an assertion: this is the only `docker run`
/// a run issues, so a failure here is a failure of the whole suite and must not
/// be reported as a schema defect.
///
/// Bounded by [`BOOT_BUDGET`] rather than by an attempt count. A name conflict
/// is not an error here — it is the expected outcome for every process but one
/// on a per-test-process runner — so losing it hands control to [`await_answer`],
/// which waits for the winner instead of racing it again.
async fn start_named() -> Option<ContainerAsync<Postgres>> {
    let deadline = std::time::Instant::now() + BOOT_BUDGET;
    loop {
        // Bound per iteration rather than carried across them: the sibling check
        // below returns without reading it, which makes a loop-scoped `last` a
        // dead assignment on that path (`-D unused-assignments`).
        let last = match test_containers::postgres_tagged(PG_TAG)
            .with_container_name(HARNESS_CONTAINER)
            .start()
            .await
        {
            Ok(container) => return Some(container),
            Err(e) => e.to_string(),
        };
        // Somebody else got the name. Wait for theirs to come up rather than
        // fight for it; only when it never does do we go round again. A daemon
        // that cannot be asked is treated as "somebody is coming up", which is
        // the non-destructive reading and the one the retry loop can recover
        // from.
        if container_verdict(HARNESS_CONTAINER) != Some(Verdict::Corpse)
            && await_answer(HARNESS_CONTAINER).await.is_some()
        {
            return None;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "postgres never started under the name {HARNESS_CONTAINER}: {last}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// The host port a named container publishes for 5432, if it is running.
///
/// Read through the `docker` CLI rather than the client library, and that is the
/// point: this is the one question that has to be answerable **without** owning
/// a `ContainerAsync`, because owning one is exactly what would remove the
/// container on drop.
fn published_port(name: &str) -> Option<u16> {
    let out = docker(&["port", name, "5432/tcp"])?;
    // `0.0.0.0:32768` or `[::]:32768`, one line per binding.
    out.lines()
        .filter_map(|line| line.rsplit(':').next())
        .find_map(|port| port.trim().parse().ok())
}

/// Does a Postgres on this port accept a connection and answer?
async fn answers(port: u16) -> bool {
    let Ok(conn) = Database::connect(small_pool(&url(port, "postgres", false))).await else {
        return false;
    };
    conn.execute_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT 1".to_owned(),
    ))
    .await
    .is_ok()
}

/// One `docker` invocation; `None` on any failure.
///
/// Deliberately silent: every caller treats "docker could not tell us" the same
/// as "there is nothing there", and falls through to starting one.
fn docker(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("docker")
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The process id a per-test database name carries, when it is one this harness
/// minted.
///
/// [`next_database`] mints `t_<pid>_<n>` and nothing else does. Both halves are
/// parsed even though only the first is returned: `t_12_notacounter` is not this
/// harness's name, and reading it as pid 12's would be a guess about somebody
/// else's database.
pub fn owning_pid(name: &str) -> Option<u32> {
    let (pid, counter) = name.strip_prefix("t_")?.split_once('_')?;
    counter.parse::<u32>().ok()?;
    pid.parse().ok()
}

/// Is a process with this id running on this host?
///
/// `None` when the question could not be **asked** — no `ps` on the path, or one
/// that fails to execute. Every caller has to treat that as "do not touch", and
/// it is a distinct value from `Some(false)` for exactly that reason.
///
/// `None` is not the only unusable answer, which is why [`prunable`] calibrates
/// rather than merely checking for it. A `ps` can be present, exit cleanly, and
/// still be answering about a different set of processes than the one the pid
/// came from — Git-Bash's on a Windows runner reports MSYS pids, so a Win32 pid
/// is reliably "not running". That arrives as `Some(false)`, indistinguishable
/// from a genuinely finished run except by asking about a process known to be
/// alive.
///
/// `ps` rather than a signal probe because this file may not add a dependency to
/// reach `kill(2)`, and rather than `/proc` because the harness runs on macOS as
/// much as on Linux.
pub fn process_is_running(pid: u32) -> Option<bool> {
    let out = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .ok()?;
    // A `ps` that lists the process prints its id; one that does not prints
    // nothing, whatever its exit status.
    Some(out.stdout.iter().any(u8::is_ascii_digit))
}

/// May this database be dropped — is it a leftover of a run that has finished?
///
/// This process's own databases need no special case and get none: this process
/// is running, so they fail the same liveness test every other live run's do. A
/// special case that can never fire is a guard nobody can prove by removing it.
///
/// **Calibrated here, not only in the caller.** The module doc claims every
/// failure mode of the liveness question lands on *keep*; that was true of
/// [`prune_stale_databases`], which asks first, and false of this function,
/// which anyone may call. The distinction stopped being academic in the donor on
/// 2026-08-19, when its tier first ran on Windows: Git-Bash puts a `ps` on the
/// runner's PATH, so the question is answerable rather than absent — and it
/// answers in an MSYS pid namespace, where a Win32 pid is nobody. Every live
/// run's database read as droppable, and the only thing standing between that
/// and a `DROP DATABASE` was the one caller that happened to calibrate. A
/// guarantee that holds because of where it is called from is a property of the
/// call site, not of the guard.
pub fn prunable(name: &str) -> bool {
    if !liveness_is_answerable() {
        return false;
    }
    owning_pid(name).is_some_and(|pid| process_is_running(pid) == Some(false))
}

/// Can this host answer the liveness question at all — does `ps` see the process
/// asking?
///
/// Memoised because the answer cannot change while this process lives, and
/// [`prunable`] is asked once per database on a shared server: recomputing it
/// would put a second `ps` behind every row of the prune.
fn liveness_is_answerable() -> bool {
    static ANSWERABLE: OnceLock<bool> = OnceLock::new();
    *ANSWERABLE.get_or_init(|| process_is_running(std::process::id()) == Some(true))
}

/// Drop the per-test databases **finished** runs left on the shared server.
///
/// See the module doc for why the owning process and not a live connection is
/// what decides that. Plain `DROP DATABASE` and never `WITH (FORCE)`, now as a
/// second line rather than as the argument.
async fn prune_stale_databases(port: u16) {
    // Calibrate before dropping anything. `prunable` now does this itself, so
    // this is no longer what makes the prune safe - it is what stops a host that
    // cannot answer the question from opening a connection to ask about every
    // database in turn and keeping none of the answers.
    if process_is_running(std::process::id()) != Some(true) {
        return;
    }
    let Ok(conn) = Database::connect(small_pool(&url(port, "postgres", false))).await else {
        return;
    };
    let Ok(rows) = conn
        .query_all_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            r"SELECT datname AS n FROM pg_database WHERE datname LIKE 't\_%'".to_owned(),
        ))
        .await
    else {
        conn.close().await.expect("close failed pruning query pool");
        return;
    };
    for row in rows {
        let Ok(name) = row.try_get::<String>("", "n") else {
            continue;
        };
        if !prunable(&name) {
            continue;
        }
        // Ignored on purpose: a database a sibling run is still connected to
        // refuses to drop, and skipping it is right even here - the run that
        // owns it has ended, but something is reading it.
        drop(
            conn.execute_raw(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!("DROP DATABASE {name}"),
            ))
            .await,
        );
    }
    conn.close().await.expect("close pruning pool");
}

/// Create this test's database, clearing a same-named leftover first.
///
/// # Why the drop is not redundant
///
/// [`next_database`] mints `t_<pid>_<n>` and the counter starts at `0` in every
/// process, so the first database of every run is deterministically
/// `t_<pid>_0`. Nothing drops databases at process exit, and [`prunable`]
/// deliberately gives this run's own pid no special case — so when the OS
/// recycles a pid (routine under `cargo nextest`, which spends one pid per
/// test, and on macOS where the pid space wraps at 99998), the prune reads a
/// dead predecessor's `t_<pid>_0` as *this* live process's and keeps it. The
/// `CREATE` then fails with `database already exists` and the run dies naming
/// the harness rather than a guard.
///
/// The drop is provably safe and is not a widening of [`prunable`]'s rule: this
/// name is one this process minted and has not yet created in this run, and no
/// *live* process can be sharing this pid. `IF EXISTS` so the ordinary case —
/// no leftover — costs one statement and no error.
async fn create_fresh_database(port: u16, database: &str) {
    create_database_as(port, database, "").await;
}

/// [`create_fresh_database`] with `options` appended to the `CREATE DATABASE`.
async fn create_database_as(port: u16, database: &str, options: &str) {
    let admin = Database::connect(small_pool(&url(port, "postgres", false)))
        .await
        .expect("connect to the maintenance database");
    admin
        .execute_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("DROP DATABASE IF EXISTS {database}"),
        ))
        .await
        .unwrap_or_else(|e| panic!("clear a recycled-pid leftover {database}: {e}"));
    admin
        .execute_raw(Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            format!("CREATE DATABASE {database}{options}"),
        ))
        .await
        .unwrap_or_else(|e| panic!("create database {database}: {e}"));
    admin.close().await.expect("close maintenance pool");
}

/// One test's own database on the shared server, carrying the applied chain.
#[derive(Clone, Debug)]
pub struct Pg {
    port: u16,
    database: String,
}

impl Pg {
    /// Create a fresh database and apply the whole migration chain to it.
    ///
    /// Applied through the toolkit runner under a `public,bss` search path,
    /// which is the arrangement this gear's chain creates its objects under:
    /// every table in `infra/storage/migrations/` names `bss.<table>` on the
    /// Postgres arm.
    pub async fn applied() -> Self {
        let port = server_port();
        let database = next_database();

        create_fresh_database(port, &database).await;

        let this = Self { port, database };
        let db = this.db().await;
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("apply the chain");
        drop(db);
        this
    }

    /// [`Pg::applied`] on a database whose locale is `C` (`LC_COLLATE` and
    /// `LC_CTYPE`), the shape `initdb --locale=C` and `CloudNativePG` give a
    /// cluster: there the database's `lower()` folds ASCII only.
    pub async fn applied_in_c_locale() -> Self {
        let port = server_port();
        let database = next_database();
        create_database_as(
            port,
            &database,
            " TEMPLATE template0 LC_COLLATE 'C' LC_CTYPE 'C'",
        )
        .await;
        let this = Self { port, database };
        let db = this.db().await;
        run_migrations_for_testing(&db, Migrator::migrations())
            .await
            .expect("apply the chain");
        drop(db);
        this
    }

    /// A fresh database with **no** chain applied — for the suites that apply or
    /// roll back the chain themselves.
    pub async fn empty() -> Self {
        let port = server_port();
        let database = next_database();
        create_fresh_database(port, &database).await;
        Self { port, database }
    }

    /// The DSN of this test's database, with or without the `public,bss` search
    /// path the chain and every repository read under.
    #[must_use]
    pub fn url(&self, search_path: bool) -> String {
        url(self.port, &self.database, search_path)
    }

    /// A toolkit [`Db`] — **its own connection pool**, under the search path.
    ///
    /// Called once per racer in a concurrency suite on purpose: two transactions
    /// taken from one pool are two server backends, but the pool is the thing
    /// that could serialize them under a small `max_connections`, and a
    /// concurrency suite must not have its concurrency supplied by luck. This is
    /// precisely what `sqlite::memory:` with `max_conns: Some(1)` cannot offer,
    /// and why this harness had to exist before any real probe could be written.
    pub async fn db(&self) -> Db {
        connect_db(
            &self.url(true),
            ConnectOpts {
                // Small, because one server carries every test's pools at once
                // and Postgres's default is a hundred backends.
                max_conns: Some(2),
                min_conns: Some(0),
                ..ConnectOpts::default()
            },
        )
        .await
        .expect("connect postgres")
    }

    /// A plain `SeaORM` connection, for the raw SQL a suite issues deliberately
    /// past every repository — the layer that cannot see a guard stop refusing,
    /// and the one an out-of-band lock observer must use.
    pub async fn raw(&self) -> DatabaseConnection {
        Database::connect(small_pool(&self.url(false)))
            .await
            .expect("connect plainly")
    }
}

/// Block until some backend **in this test's own database** is waiting on a lock
/// it has not been granted.
///
/// This is what turns a two-task race into a race rather than a coin toss: a
/// backend in a lock wait has already executed everything before the statement
/// that blocked, so observing it proves the loser's statement reached the
/// contested resource *before* the winner committed. Without it the loser could
/// read the winner's committed state, be refused by its own precondition, and
/// the test would be green about nothing.
///
/// **Narrowed to `current_database()`**, which the shared-server harness makes
/// necessary: `pg_locks` is server-wide and a sibling test blocking in its own
/// database would otherwise satisfy this wait. The narrowing goes through
/// `pg_stat_activity` rather than `pg_locks.database`, because the wait a
/// duplicate key or a row lock produces is `locktype = 'transactionid'` and that
/// row's `database` is NULL.
///
/// It is deliberately **not** narrowed by relation: filtering to one table's oid
/// would return zero forever and every wait would time out on a race that was
/// working perfectly.
///
/// # Panics
/// After fifteen seconds, because a race that never contends is a refuted claim
/// and not a slow one.
pub async fn wait_until_a_backend_blocks(conn: &DatabaseConnection) {
    for _ in 0..600_u32 {
        if blocked_backends(conn).await > 0 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("no backend ever blocked: the two statements did not contend");
}

/// How many backends of this database are waiting on a lock they do not hold.
pub async fn blocked_backends(conn: &DatabaseConnection) -> i64 {
    conn.query_one_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        "SELECT count(*)::bigint AS n
           FROM pg_locks l
           JOIN pg_stat_activity a ON a.pid = l.pid
          WHERE NOT l.granted AND a.datname = current_database()"
            .to_owned(),
    ))
    .await
    .expect("query pg_locks")
    .expect("one row")
    .try_get::<i64>("", "n")
    .expect("read the count")
}

/// A database name unique to this process and this call.
///
/// The process id is in it because the server is shared across binaries and
/// across runs, so a bare counter would collide with a sibling's `t_0` on its
/// very first test.
fn next_database() -> String {
    format!(
        "t_{}_{}",
        std::process::id(),
        NEXT_DATABASE.fetch_add(1, Ordering::Relaxed)
    )
}

fn url(port: u16, database: &str, search_path: bool) -> String {
    let suffix = if search_path {
        "?options=-c%20search_path%3Dpublic,bss"
    } else {
        ""
    };
    format!("postgres://postgres:postgres@127.0.0.1:{port}/{database}{suffix}")
}

fn small_pool(url: &str) -> ConnectOptions {
    let mut options = ConnectOptions::new(url.to_owned());
    options.max_connections(2).min_connections(0);
    options
}
