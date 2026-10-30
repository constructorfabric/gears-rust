//! The startup task (`docs/DESIGN.md` §3.1): `start` returns at once, and this
//! task connects, checks, migrates and starts the `LISTEN` task, the
//! notifier where `head_notify` asks for it (§2.3) and the maintenance
//! (§4.9), retrying with backoff until it succeeds or the backend
//! is stopped. Until then every operation is `Unavailable`.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use rand::RngExt;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, PgPool};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::settings::{HeadNotify, PostgresSettings};
use crate::shared::Shared;
use crate::{listener, maintenance, notify};

/// The oldest server the plugin runs on.
const MIN_SERVER_VERSION: i32 = 140_000;

const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Starts the startup task on `tasks`; it retries with backoff until it
/// succeeds or `stop` fires.
pub fn spawn(shared: Arc<Shared>, stop: CancellationToken, tasks: &TaskTracker) {
    let worker_tasks = tasks.clone();
    tasks.spawn(async move {
        let mut backoff = BACKOFF_FIRST;
        loop {
            match start(&shared).await {
                Ok((main, background)) => {
                    shared.set_pool(main.clone());
                    tracing::info!(schema = %shared.settings.schema, role = ?shared.role, "postgres event-broker backend started");
                    listener::spawn(Arc::clone(&shared), stop.clone(), &worker_tasks);
                    if shared.settings.head_notify == HeadNotify::AfterCommit {
                        notify::spawn(&shared.heads_to_notify, &main, &stop, &worker_tasks);
                    }
                    if let Some(background) = background {
                        shared.set_background(background.clone());
                        maintenance::start::spawn(&shared, &background, &stop, &worker_tasks);
                    }
                    return;
                }
                Err(problem) => {
                    tracing::warn!(%problem, retry_in = ?backoff, "postgres event-broker backend not started yet");
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
pub fn jittered(backoff: Duration) -> Duration {
    backoff.mul_f64(rand::rng().random_range(0.5..=1.0))
}

/// The options of every connection this plugin opens, named `application`
/// in `pg_stat_activity`.
pub fn connect_options(
    settings: &PostgresSettings,
    application: &str,
) -> Result<PgConnectOptions, sqlx::Error> {
    Ok(PgConnectOptions::from_str(&settings.url)?.application_name(application))
}

/// A pool of `size` connections, each set up for the plugin's schema.
fn pool_options(settings: &PostgresSettings, size: u32) -> PgPoolOptions {
    let schema = settings.schema.clone();
    PgPoolOptions::new()
        .max_connections(size)
        .acquire_timeout(settings.pool_acquire_timeout)
        .after_connect(move |conn, _meta| {
            let schema = schema.clone();
            Box::pin(async move {
                sqlx::raw_sql(AssertSqlSafe(format!(
                    "SET synchronous_commit = on; SET search_path TO {schema}; \
                     SET lock_timeout = '5s'; SET idle_in_transaction_session_timeout = '30s'"
                )))
                .execute(conn)
                .await?;
                Ok(())
            })
        })
}

/// The main pool, connected, checked and migrated, and the background pool
/// where this process runs the maintenance.
async fn start(shared: &Shared) -> Result<(PgPool, Option<PgPool>), String> {
    let settings = &shared.settings;
    let options =
        connect_options(settings, "evbk-main").map_err(|_| "the url does not parse".to_owned())?;
    let pool = pool_options(settings, settings.pools.main)
        .connect_with(options.clone())
        .await
        .map_err(|e| format!("connect: {e}"))?;

    let version: String = sqlx::query_scalar("SHOW server_version_num")
        .fetch_one(&pool)
        .await
        .map_err(|e| format!("server version: {e}"))?;
    let version: i32 = version
        .parse()
        .map_err(|_| "server_version_num does not parse".to_owned())?;
    if version < MIN_SERVER_VERSION {
        return Err(format!(
            "PostgreSQL {version} is older than the minimum {MIN_SERVER_VERSION} (14)"
        ));
    }
    let isolation: String = sqlx::query_scalar("SHOW transaction_isolation")
        .fetch_one(&pool)
        .await
        .map_err(|e| format!("isolation: {e}"))?;
    if isolation != "read committed" {
        return Err(format!(
            "the default isolation is {isolation}; read committed is required"
        ));
    }
    sqlx::query(AssertSqlSafe(format!(
        "CREATE SCHEMA IF NOT EXISTS {}",
        settings.schema
    )))
    .execute(&pool)
    .await
    .map_err(|e| format!("schema: {e}"))?;
    sqlx::migrate!("./src/migrations")
        .run(&pool)
        .await
        .map_err(|e| format!("migrations: {e}"))?;
    // Connects on first use: the main pool has just proven the database
    // reachable.
    let background = shared.runs_maintenance().then(|| {
        pool_options(settings, settings.pools.background)
            .connect_lazy_with(options.clone().application_name("evbk-background"))
    });
    Ok((pool, background))
}
