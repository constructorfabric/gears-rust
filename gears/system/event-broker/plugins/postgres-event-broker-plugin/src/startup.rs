//! The startup task (`docs/DESIGN.md` §3.1): `build` returns at once, and this
//! task connects, checks, migrates and starts the workers, retrying with
//! backoff until it succeeds or the backend is stopped. Until then every
//! operation is `Unavailable`.

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use rand::RngExt;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{AssertSqlSafe, PgPool};
use tokio_util::sync::CancellationToken;

use crate::shared::Shared;
use crate::{listener, maintenance};

/// The oldest server the plugin runs on: `DETACH PARTITION ... CONCURRENTLY`.
const MIN_SERVER_VERSION: i32 = 140_000;

const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

pub fn spawn(shared: Arc<Shared>, stop: CancellationToken) {
    tokio::spawn(async move {
        let mut backoff = BACKOFF_FIRST;
        loop {
            match start(&shared).await {
                Ok(pool) => {
                    shared.set_pool(pool);
                    tracing::info!(schema = %shared.settings.schema, "postgres event-broker backend started");
                    listener::spawn(Arc::clone(&shared), stop.clone());
                    maintenance::spawn(Arc::clone(&shared), stop);
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

async fn start(shared: &Shared) -> Result<PgPool, String> {
    let settings = &shared.settings;
    let options = PgConnectOptions::from_str(&settings.url)
        .map_err(|_| "the url does not parse".to_owned())?;
    let schema = settings.schema.clone();
    let pool = PgPoolOptions::new()
        .max_connections(settings.pool_max_size)
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
        .connect_with(options)
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
    Ok(pool)
}
