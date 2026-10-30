//! The startup task (`docs/DESIGN.md` §3.1): `start` returns at once, and this
//! task connects, detects the dialect, checks the server, creates the
//! database, migrates, and starts the head poll and the maintenance (§4.9),
//! retrying with backoff until it succeeds or the backend is stopped. Until
//! then every operation is `Unavailable`.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use rand::RngExt;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::{AssertSqlSafe, Connection, MySqlConnection, MySqlPool};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::dialect::{Dialect, Durability, Topology};
use crate::settings::MysqlSettings;
use crate::shared::Shared;
use crate::{error, heads, maintenance};

/// How long one connection attempt may take: an address that drops packets
/// would otherwise hold startup for the system's TCP timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How long a row lock is waited for, in seconds: a retry blocked behind a
/// hung append's fence row fails rather than waiting for the server's
/// default of 50 s.
const ROW_LOCK_WAIT: u32 = 5;

/// Starts the startup task on `tasks`; it retries with backoff until it
/// succeeds or `stop` fires.
pub fn spawn(shared: Arc<Shared>, stop: CancellationToken, tasks: &TaskTracker) {
    let worker_tasks = tasks.clone();
    tasks.spawn(async move {
        let mut backoff = BACKOFF_FIRST;
        loop {
            // A stop drops the attempt where it stands, closing what it opened.
            let attempt = tokio::select! {
                () = stop.cancelled() => return,
                attempt = start(&shared) => attempt,
            };
            match attempt {
                Ok((dialect, main, background)) => {
                    if !shared.install(main.clone(), background.clone()) {
                        return;
                    }
                    tracing::info!(database = %shared.settings.database, %dialect, role = ?shared.role,
                        "mysql event-broker backend started");
                    heads::spawn(&shared, &main, &stop, &worker_tasks);
                    if let Some(background) = background {
                        maintenance::start::spawn(&shared, &background, &stop, &worker_tasks);
                    }
                    return;
                }
                Err(problem) => {
                    tracing::warn!(%problem, retry_in = ?backoff, "mysql event-broker backend not started yet");
                    shared.set_problem(problem);
                }
            }
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(jittered(backoff)) => {}
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    });
}

/// Between half and all of `backoff`, so a fleet does not retry in step.
fn jittered(backoff: Duration) -> Duration {
    backoff.mul_f64(rand::rng().random_range(0.5..=1.0))
}

/// The options of every connection this plugin opens: UTC, which `DATETIME`
/// columns are read and written in.
fn connect_options(settings: &MysqlSettings) -> Result<MySqlConnectOptions, sqlx::Error> {
    Ok(MySqlConnectOptions::from_str(&settings.url)?.timezone(Some("+00:00".to_owned())))
}

/// A pool of `size` connections in the plugin's database, each in
/// `READ COMMITTED`: no gap locks, and each statement sees what committed
/// before it.
fn pool_options(settings: &MysqlSettings, size: u32) -> MySqlPoolOptions {
    MySqlPoolOptions::new()
        .max_connections(size)
        .acquire_timeout(settings.pool_acquire_timeout)
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                sqlx::raw_sql(AssertSqlSafe(format!(
                    "SET SESSION TRANSACTION ISOLATION LEVEL READ COMMITTED; \
                     SET SESSION innodb_lock_wait_timeout = {ROW_LOCK_WAIT}"
                )))
                .execute(conn)
                .await?;
                Ok(())
            })
        })
}

/// Connected, checked and migrated: the dialect, the main pool, and the
/// background pool where this process runs the maintenance.
async fn start(shared: &Shared) -> Result<(Dialect, MySqlPool, Option<MySqlPool>), String> {
    let settings = &shared.settings;
    let options = connect_options(settings).map_err(|_| "the url does not parse".to_owned())?;
    let failed = |step: &str, e: &sqlx::Error| match error::number(e) {
        Some(number) => format!("{step}: database error {number}"),
        None => format!("{step}: {}", error::kind(e)),
    };

    // The server first, with the url's own database, if any: the plugin's may
    // not exist yet.
    let mut conn = tokio::time::timeout(CONNECT_TIMEOUT, MySqlConnection::connect_with(&options))
        .await
        .map_err(|_| "connect: timed out".to_owned())?
        .map_err(|e| failed("connect", &e))?;
    let dialect = check(&mut conn, settings).await?;
    sqlx::query(AssertSqlSafe(format!(
        "CREATE DATABASE IF NOT EXISTS {} CHARACTER SET utf8mb4 COLLATE utf8mb4_bin",
        settings.database
    )))
    .execute(&mut conn)
    .await
    .map_err(|e| failed("create database", &e))?;
    if conn.close().await.is_err() {
        tracing::debug!("mysql event-broker backend: the setup connection did not close cleanly");
    }

    let options = options.database(&settings.database);
    let pool = tokio::time::timeout(
        CONNECT_TIMEOUT,
        pool_options(settings, settings.pools.main).connect_with(options.clone()),
    )
    .await
    .map_err(|_| "connect: timed out".to_owned())?
    .map_err(|e| failed("connect", &e))?;
    let isolation: String = sqlx::query_scalar(dialect.isolation_query())
        .fetch_one(&pool)
        .await
        .map_err(|e| failed("isolation", &e))?;
    if isolation != "READ-COMMITTED" {
        return Err(format!("sessions run at {isolation}, not READ-COMMITTED"));
    }
    sqlx::migrate!("./src/migrations")
        .run(&pool)
        .await
        .map_err(|e| format!("migrations: {e}"))?;
    // Connects on first use: the main pool has just proven the server
    // reachable.
    let background = shared.runs_maintenance().then(|| {
        pool_options(settings, settings.pools.background).connect_lazy_with(options.clone())
    });
    Ok((dialect, pool, background))
}

/// The dialect, refused below its engine's floor; the durability variables,
/// refused when `require_durable_server` is on and a commit would not be
/// durable, and warned of when it is off.
async fn check(conn: &mut MySqlConnection, settings: &MysqlSettings) -> Result<Dialect, String> {
    let version: String = sqlx::query_scalar("SELECT VERSION()")
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| format!("server version: {}", error::kind(&e)))?;
    let dialect = Dialect::parse(&version)
        .ok_or_else(|| "the server's version does not parse as MySQL or MariaDB".to_owned())?;
    if !dialect.is_supported() {
        return Err(format!(
            "{dialect} is older than the oldest supported release, {}",
            dialect.floor()
        ));
    }
    let variables: Vec<(String, String)> = sqlx::query_as(Topology::QUERY)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| format!("replication variables: {}", error::kind(&e)))?;
    let value = |name: &str| {
        variables
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
    };
    let topology = Topology {
        wsrep_on: value("wsrep_on"),
        group_replication_single_primary: value("group_replication_single_primary_mode"),
    };
    if let Some(refusal) = topology.refusal() {
        return Err(format!("unsupported topology: {refusal}"));
    }
    let (flush_log_at_trx_commit, log_bin, sync_binlog, binlog_format): (i64, i64, i64, String) =
        sqlx::query_as(Durability::QUERY)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| format!("durability variables: {}", error::kind(&e)))?;
    let durability = Durability {
        flush_log_at_trx_commit,
        log_bin: log_bin != 0,
        sync_binlog,
        binlog_format,
    };
    if durability.refuses_read_committed() {
        return Err(
            "the binary log is statement-based, which refuses READ COMMITTED writes; set binlog_format to ROW or MIXED"
                .to_owned(),
        );
    }
    let shortfalls = durability.shortfalls(dialect);
    if !shortfalls.is_empty() {
        if settings.require_durable_server {
            return Err(format!(
                "the server does not make every commit durable ({}); fix the server or set require_durable_server to false",
                shortfalls.join("; ")
            ));
        }
        tracing::warn!(shortfalls = ?shortfalls, "mysql event-broker backend: the server does not make every commit durable; an acknowledged append may be lost in a crash");
    }
    Ok(dialect)
}
