//! One Postgres server per test **binary**, one `CREATE DATABASE` per test.
//!
//! # Why this exists rather than a container per test
//!
//! The Postgres suites of this phase were first written starting a fresh
//! container inside every test. That does not scale and it
//! does not merely cost time: Track P2 measured sporadic
//! `PortNotExposed { port: Tcp(5432) }` panics in whichever tests happened to be
//! starting at the same moment — the daemon reports a container up a beat before
//! its port binding is readable — and eleven such false positives showed up in
//! that track's first guard-by-removal pass.
//!
//! Under a guard-by-removal discipline a *spurious* red is worse than a slow
//! run. The whole proof is "delete the guard, watch **exactly one** test fail",
//! and a flake is indistinguishable from the second test a removal was not
//! supposed to redden. So the harness itself has to be the least flaky thing in
//! the run.
//!
//! A fresh **database** per test buys the same isolation for a fraction of the
//! cost: every test still gets a virgin schema with no row any other test wrote,
//! so tests may keep reusing one handful of fixed ids. `postgres_schema_price.rs`
//! measured 52 tests in 1.8s instead of 12s, and zero flakes over five runs.
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
//! every Postgres binary therefore left a live Postgres behind.
//!
//! **That is not a tidiness point; it is the same defect this file exists to
//! fix.** In one working session the leak filled the Docker VM's disk, and the
//! next run answered with fifteen `No space left on device` failures spread over
//! one schema suite — fifteen reds that said nothing about any schema. A harness
//! whose own failures are indistinguishable from the guard failures it exists to
//! prove is worse than a slow one.
//!
//! So the container carries a fixed name, [`HARNESS_CONTAINER`], and is
//! **reused**: a binary that finds it already running and answering connects to
//! it rather than starting another. The leak is bounded at exactly one
//! container, forever, instead of one per binary per run — and the four Postgres
//! binaries of a full run now share a single server.
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
//! `{port, database}` and owns no pool, [`Pg::applied`] drops the pool it built
//! for the migration chain, and `postgres_approval_race.rs` has no connection at
//! all open between `seed()` and `submit()`. Two concurrent `cargo test`
//! invocations — the case this file explicitly claims to hold — could therefore
//! destroy each other's databases mid-run, producing exactly the red that says
//! nothing about any guard that this file exists to eliminate.
//!
//! What replaces it is the process id already in the name. `next_database` mints
//! `t_<pid>_<n>`, so every database says which run made it, and
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
//! # Contention is the normal case here, not the exotic one
//!
//! This paragraph used to read: "`cargo test` runs test *binaries* sequentially,
//! so this gear's suites do not normally contend for the shared server; two
//! concurrent `cargo test` invocations would." **The premise was true and the
//! conclusion was not**, because nothing runs this tier with `cargo test`:
//! `make test-pricing-pg` runs `cargo nextest`, and nextest gives every *test*
//! its own process. It is not four binaries in sequence contending never — it is
//! as many processes as the runner has cores, racing for one fixed container
//! name, on every run.
//!
//! That mis-sizing cost a red on 2026-08-19, the first time this tier ever
//! executed in CI: three of 380 failed at 3.03s each on Docker `409 Conflict`
//! for the container name, while the other 377 passed against the container the
//! winner had by then finished booting. The budget in [`start_named`] was five
//! attempts 500ms apart — about 2.5s of waiting — which is less than a cold
//! runner needs to pull the image and let Postgres accept a first connection.
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
//! port, force-removes nothing, fails to start under a name that is already
//! taken, and panics in `start_named` after five attempts. Deterministically,
//! and with a message about Postgres rather than about Docker. It is reported
//! rather than closed: the check would be a third way of asking the same
//! question, and the failure is loud and repeatable rather than silent.
//!
//! # What a suite must still do for itself
//!
//! `must_be_rejected` is deliberately **not** here. Every suite asserts that a
//! refusal is *the one under test* — a bare "some error happened" would pass
//! with the guard it means to prove switched off — and the fragment that makes
//! that assertion sharp differs per suite: a constraint name here, a trigger's
//! literal message there. Hoisting them into one helper means taking the weakest
//! of them, which is how a suite ends up green against a schema that no longer
//! holds.
//!
//! **Anything a test reads out of a server-wide catalog must be narrowed to
//! `current_database()`.** One server now carries every test's database at once,
//! so `pg_locks`, `pg_stat_activity` and friends see other tests' backends.
//! `postgres_audit_chain.rs`'s lock observer is the live instance of this.

#![allow(
    dead_code,
    reason = "each test binary compiles the whole module and uses part of it"
)]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use bss_pricing::infra::storage::migrations::Migrator;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use sea_orm_migration::MigratorTrait;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::testcontainers::runners::AsyncRunner;
use testcontainers_modules::testcontainers::{ContainerAsync, ImageExt};
use toolkit_db::migration_runner::run_migrations_for_testing;
use toolkit_db::{ConnectOpts, Db, connect_db};

/// The one container's name — fixed, so a later run finds it instead of
/// starting another. See the module doc for why reuse rather than cleanup.
pub const HARNESS_CONTAINER: &str = "bss-pricing-pg-harness";

/// The mapped port of the one server, resolved on first use.
static SERVER: OnceLock<u16> = OnceLock::new();

/// Names the per-test databases apart. An atomic rather than a uuid, so a
/// failure message points at a database a human can go and look at; the process
/// id is in there too, because the server outlives the process now.
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
    // Something under our name is not answering *yet*. Two cases, and conflating
    // them is what turns one lost race into a cascade: a container a killed run
    // left half-started, and a sibling's container still booting Postgres. Ask
    // the daemon which, rather than inferring it from the silence.
    if container_is_running(HARNESS_CONTAINER)
        && let Some(port) = await_answer(HARNESS_CONTAINER).await
    {
        return (port, None);
    }
    // Not running, or running and never came up inside the budget: a corpse.
    // Removing it by name is safe — the name is this harness's own.
    let _ = docker(&["rm", "-f", HARNESS_CONTAINER]);
    if let Some(container) = start_named().await {
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("map the postgres port");
        return (port, Some(container));
    }
    // A sibling binary won the race and started it under our name. It may still
    // be booting, so wait rather than read a port it has not published yet.
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
/// initialising a cluster while a dozen sibling processes watch. The five
/// attempts 500ms apart this replaced covered 2.5s, and 2.5s was not enough.
const BOOT_BUDGET: std::time::Duration = std::time::Duration::from_secs(90);

/// Is a container with this name in the daemon's `running` state?
///
/// The question "is it answering" cannot tell a booting container from a dead
/// one; this can, and it is the whole reason the force-remove is now conditional.
fn container_is_running(name: &str) -> bool {
    docker(&["inspect", "-f", "{{.State.Running}}", name]).is_some_and(|out| out.trim() == "true")
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
        if std::time::Instant::now() >= deadline || !container_is_running(name) {
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
/// is not an error here — it is the expected outcome for every process but one,
/// on every nextest run — so losing it hands control to [`await_answer`], which
/// waits for the winner instead of racing it again.
async fn start_named() -> Option<ContainerAsync<Postgres>> {
    let deadline = std::time::Instant::now() + BOOT_BUDGET;
    loop {
        // Bound per iteration rather than carried across them: the sibling check
        // below returns without reading it, which makes a loop-scoped `last` a
        // dead assignment on that path (`-D unused-assignments`).
        let last = match cf_gears_test_containers::postgres()
            .with_container_name(HARNESS_CONTAINER)
            .start()
            .await
        {
            Ok(container) => return Some(container),
            Err(e) => e.to_string(),
        };
        // Somebody else got the name. Wait for theirs to come up rather than
        // fight for it; only when it never does do we go round again.
        if container_is_running(HARNESS_CONTAINER)
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
/// [`prune_stale_databases`], which asks first, and false of this function, which
/// anyone may call. The distinction stopped being academic on 2026-08-19, when
/// this tier first ran on Windows: Git-Bash puts a `ps` on the runner's PATH, so
/// the question is answerable rather than absent — and it answers in an MSYS pid
/// namespace, where a Win32 pid is nobody. Every live run's database read as
/// droppable, and the only thing standing between that and a `DROP DATABASE` was
/// the one caller that happened to calibrate. A guarantee that holds because of
/// where it is called from is a property of the call site, not of the guard.
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
    // this is no longer what makes the prune safe — it is what stops a host that
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
        // refuses to drop, and skipping it is right even here — the run that
        // owns it has ended, but something is reading it.
        drop(
            conn.execute_raw(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!("DROP DATABASE {name}"),
            ))
            .await,
        );
    }
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
    /// which is the arrangement `postgres_migrations.rs` establishes as the one
    /// production boots cleanly under.
    pub async fn applied() -> Self {
        let port = server_port();
        let database = next_database();

        let admin = Database::connect(small_pool(&url(port, "postgres", false)))
            .await
            .expect("connect to the maintenance database");
        admin
            .execute_raw(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!("CREATE DATABASE {database}"),
            ))
            .await
            .unwrap_or_else(|e| panic!("create database {database}: {e}"));
        drop(admin);

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
        let admin = Database::connect(small_pool(&url(port, "postgres", false)))
            .await
            .expect("connect to the maintenance database");
        admin
            .execute_raw(Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!("CREATE DATABASE {database}"),
            ))
            .await
            .unwrap_or_else(|e| panic!("create database {database}: {e}"));
        drop(admin);
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
    /// Called once per racer in the concurrency suite on purpose: two
    /// transactions taken from one pool are two server backends, but the pool is
    /// the thing that could serialize them under a small `max_connections`, and
    /// a concurrency suite must not have its concurrency supplied by luck.
    pub async fn db(&self) -> Db {
        connect_db(
            &self.url(true),
            ConnectOpts {
                // Small, because one server now carries every test's pools at
                // once and Postgres's default is a hundred backends.
                max_conns: Some(2),
                min_conns: Some(0),
                ..ConnectOpts::default()
            },
        )
        .await
        .expect("connect postgres")
    }

    /// A plain `SeaORM` connection, for the raw SQL a schema suite issues
    /// deliberately past every repository — the layer that cannot see a guard
    /// stop refusing.
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
/// that blocked, so observing it proves the loser's read happened *before* the
/// winner committed. Without it the loser could read the winner's committed
/// state, both would succeed, and the test would be green about nothing.
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
/// The process id is in it because the server is now shared across binaries and
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

// ---------------------------------------------------------------------------
// The frozen-column census, parameterised on the table
// ---------------------------------------------------------------------------

/// What a table's append-only guard owes, read off the **table** rather than off
/// the guard.
///
/// A whitelist enumerated by hand cannot notice a column added later, and this
/// crate keeps producing exactly that defect: `trg_pricing_price_append_only` paid for the tax
/// columns, `000051` for the proration ones, `000055` for the reservation pair,
/// `000057` for the floors and `000069` for the `per_unit` rate — five waves in
/// which a column arrived and its guard line did not. A census cross-checked
/// against a **count** is the same blindness one layer up: a 46th column added to
/// `pricing_price` and forgotten moves neither a hand-written array nor the
/// literal beside it, and the column becomes mutable under a frozen
/// `CatalogVersion` with both green.
///
/// So the owed set is derived from `information_schema.columns` and nothing here
/// is counted. See [`frozen_columns`] for the arm slicing, which is the only part
/// a caller supplies an anchor for.
pub struct FrozenColumns {
    /// Every column the guard must freeze: the table's own columns, less the
    /// ones the design set sanctions as mutable on a frozen row.
    pub owed: Vec<String>,
    /// The text of the guard's frozen-column arm, sliced out of the function
    /// body.
    pub predicate: String,
}

impl FrozenColumns {
    /// The owed columns the arm does not name — empty is the passing state.
    #[must_use]
    pub fn missing(&self) -> Vec<&str> {
        self.owed
            .iter()
            .map(String::as_str)
            // The trailing space is what keeps `min_qty_usage` from matching
            // `min_qty_usage_fallback`'s line, and `plan_tier` from matching
            // `plan_tier_override`'s.
            .filter(|column| !self.predicate.contains(&format!("NEW.{column} ")))
            .collect()
    }
}

/// Read a table's frozen-column census off the catalog.
///
/// `arm_opens_on` is the first `IF NEW.<column>` of the frozen-column arm, and
/// the slicing it drives is load-bearing rather than tidy: these guard functions
/// carry several arms and a comment block, so a column named in the `DELETE` ban,
/// in a lifecycle whitelist or in a comment would satisfy a function-wide match
/// while being unguarded. `bss.pricing_price_append_only()` shows the hazard
/// concretely — `grandfather_until` appears in its **monotonicity** arm and not in
/// the frozen-column one, so a function-wide `contains` would report it frozen
/// when it is not.
///
/// # Panics
/// When the function, the arm opener or the arm's closing `THEN` is absent: each
/// of those is a guard that no longer has the shape this census reads, and a
/// census that quietly returned an empty predicate would report every column
/// unguarded — or, worse, be silenced with an exemption.
pub async fn frozen_columns(
    conn: &DatabaseConnection,
    table: &str,
    guard_function: &str,
    arm_opens_on: &str,
    sanctioned_mutable: &[&str],
) -> FrozenColumns {
    let owed = catalog_strings(
        conn,
        &format!(
            "SELECT column_name AS v FROM information_schema.columns \
             WHERE table_schema = 'bss' AND table_name = '{table}' ORDER BY 1"
        ),
    )
    .await
    .into_iter()
    .filter(|column| !sanctioned_mutable.contains(&column.as_str()))
    .collect();

    let bodies = catalog_strings(
        conn,
        &format!(
            "SELECT prosrc AS v FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = 'bss' AND p.proname = '{guard_function}'"
        ),
    )
    .await;
    let body = bodies
        .first()
        .unwrap_or_else(|| panic!("the guard function bss.{guard_function}() must exist"));
    let arm_start = body.find(arm_opens_on).unwrap_or_else(|| {
        panic!("the frozen-column arm of bss.{guard_function}() must open on `{arm_opens_on}`")
    });
    let arm = &body[arm_start..];
    let arm_end = arm
        .find(" THEN")
        .unwrap_or_else(|| panic!("the frozen-column arm of bss.{guard_function}() must close"));

    FrozenColumns {
        owed,
        predicate: arm[..arm_end].to_owned(),
    }
}

/// One text column of a catalog query, in order.
pub async fn catalog_strings(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    conn.query_all_raw(Statement::from_string(
        sea_orm::DatabaseBackend::Postgres,
        sql.to_owned(),
    ))
    .await
    .expect("run the catalog query")
    .iter()
    .map(|row| row.try_get::<String>("", "v").expect("read the value"))
    .collect()
}
