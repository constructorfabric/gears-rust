//! The startup task (`docs/DESIGN.md` §3.1): `start` returns at once, and this
//! task opens the file, migrates it and starts the sweep, retrying with
//! backoff until it succeeds or the backend is stopped. Until then every
//! operation is `Unavailable`.

use std::sync::Arc;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::error::{self, Operation};
use crate::hub;
use crate::settings::{SqliteSettings, Synchronous};
use crate::shared::{Opened, Shared};
use crate::sweep;

const BACKOFF_FIRST: Duration = Duration::from_millis(250);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Starts the startup task on `tasks`.
pub fn spawn(shared: Arc<Shared>, stop: CancellationToken, tasks: &TaskTracker) {
    let worker_tasks = tasks.clone();
    tasks.spawn(async move {
        let mut backoff = BACKOFF_FIRST;
        loop {
            let opened = tokio::select! {
                () = stop.cancelled() => return,
                opened = open(&shared.settings) => opened,
            };
            match opened {
                Ok(opened) => {
                    let pool = opened.pool.clone();
                    let hub = Arc::clone(&opened.hub);
                    shared.set_opened(opened);
                    tracing::info!(role = ?shared.role, "sqlite event-broker backend started");
                    if shared.runs_sweep() {
                        sweep::spawn(&shared, pool, hub, &stop, &worker_tasks);
                    }
                    return;
                }
                Err(problem) => {
                    tracing::warn!(%problem, retry_in = ?backoff, "sqlite event-broker backend not started yet");
                }
            }
            tokio::select! {
                () = stop.cancelled() => return,
                () = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(BACKOFF_MAX);
        }
    });
}

/// The options of every connection: WAL, so readers never wait for the
/// writer; incremental auto-vacuum, so the sweep can hand freed pages back.
fn connect_options(settings: &SqliteSettings) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(&settings.path)
        .create_if_missing(true)
        .auto_vacuum(SqliteAutoVacuum::Incremental)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(match settings.synchronous {
            Synchronous::Full => SqliteSynchronous::Full,
            Synchronous::Normal => SqliteSynchronous::Normal,
        })
        .busy_timeout(settings.busy_timeout)
        .foreign_keys(true)
}

/// The file opened and migrated. The problem names what failed and never
/// quotes the driver, whose text carries the path.
async fn open(settings: &SqliteSettings) -> Result<Opened, String> {
    let path = settings.path.clone();
    let hub = tokio::task::spawn_blocking(move || hub::of(&path))
        .await
        .map_err(|_| "resolving `path` did not finish".to_owned())?
        .map_err(|e| format!("the directory of `path` cannot be resolved ({})", e.kind()))?;
    let opening = hub.opening.lock().await;
    let pool = SqlitePoolOptions::new()
        .max_connections(settings.connections)
        .acquire_timeout(settings.pool_acquire_timeout)
        .connect_with(connect_options(settings))
        .await
        .map_err(|e| error::map(Operation::Open, &e).to_string())?;
    sqlx::migrate!("./src/migrations")
        .run(&pool)
        .await
        .map_err(|e| match e {
            sqlx::migrate::MigrateError::Execute(e) => error::map(Operation::Open, &e).to_string(),
            other => format!(
                "migrations: {}",
                match other {
                    sqlx::migrate::MigrateError::VersionMismatch(_) =>
                        "an applied migration no longer matches",
                    sqlx::migrate::MigrateError::VersionMissing(_) =>
                        "the file holds a migration this plugin does not know",
                    sqlx::migrate::MigrateError::Dirty(_) =>
                        "a migration was left partially applied",
                    _ => "failed",
                }
            ),
        })?;
    drop(opening);
    Ok(Opened { pool, hub })
}
