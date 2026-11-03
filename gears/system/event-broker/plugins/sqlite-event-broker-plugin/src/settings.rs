//! A `SQLite` registry entry's settings (`docs/DESIGN.md` §5).
//!
//! Read by hand rather than through serde, so an error names the setting and
//! the rule it breaks and never quotes a value.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use event_broker_sdk::backend::BackendError;
use toolkit_utils::var_expand::expand_env_vars;

/// When a commit reaches the disk (`PRAGMA synchronous`, `docs/DESIGN.md` §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Synchronous {
    /// Every commit is synced to disk before it returns: an acknowledged
    /// append survives a power loss.
    Full,
    /// The log is synced at checkpoints only: an acknowledged append survives
    /// a crash of the process, not of the machine.
    Normal,
}

/// The retention sweep (`docs/DESIGN.md` §4.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sweep {
    /// Between two passes over every bounded partition.
    pub interval: Duration,
    /// The most events one write transaction of the sweep removes.
    pub batch: u32,
}

/// One registry entry's settings, defaults applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqliteSettings {
    /// The database file, `${VAR}` expanded; its directory must exist.
    pub path: PathBuf,
    /// When a commit reaches the disk.
    pub synchronous: Synchronous,
    /// How long a statement waits for a lock another connection holds.
    pub busy_timeout: Duration,
    /// The size of the connection pool.
    pub connections: u32,
    /// How long an operation waits for a pooled connection.
    pub pool_acquire_timeout: Duration,
    /// The retention sweep.
    pub sweep: Sweep,
}

impl SqliteSettings {
    /// Reads an entry's settings; an unknown setting or a broken rule is
    /// `InvalidConfig`.
    pub fn parse(
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, BackendError> {
        let mut path = None;
        let mut parsed = Self {
            path: PathBuf::new(),
            synchronous: Synchronous::Full,
            busy_timeout: Duration::from_secs(5),
            connections: 4,
            pool_acquire_timeout: Duration::from_secs(5),
            sweep: Sweep {
                interval: Duration::from_secs(1),
                batch: 10_000,
            },
        };
        for (name, value) in settings {
            match name.as_str() {
                "path" => path = Some(file_path(value)?),
                "synchronous" => {
                    parsed.synchronous = match value.as_str() {
                        Some("full") => Synchronous::Full,
                        Some("normal") => Synchronous::Normal,
                        _ => return Err(invalid("`synchronous` must be \"full\" or \"normal\"")),
                    };
                }
                "busy_timeout_ms" => parsed.busy_timeout = millis(value, "busy_timeout_ms")?,
                "connections" => parsed.connections = positive(value, "connections")?,
                "pool_acquire_timeout_ms" => {
                    parsed.pool_acquire_timeout = millis(value, "pool_acquire_timeout_ms")?;
                }
                "sweep" => parsed.sweep = sweep(value, parsed.sweep)?,
                other => {
                    return Err(invalid(format!(
                        "`{other}` is not a sqlite backend setting"
                    )));
                }
            }
        }
        parsed.path = path.ok_or_else(|| invalid("`path` is required"))?;
        Ok(parsed)
    }
}

fn invalid(detail: impl fmt::Display) -> BackendError {
    BackendError::invalid_config(detail)
}

/// A plain file path: `:memory:` and `file:` URIs would bypass the file this
/// plugin's durability and single-owner rules are about.
fn file_path(value: &serde_json::Value) -> Result<PathBuf, BackendError> {
    let raw = value
        .as_str()
        .ok_or_else(|| invalid("`path` must be a string"))?;
    let expanded = expand_env_vars(raw)
        .map_err(|_| invalid("`path` names an environment variable that is not set"))?;
    if expanded.is_empty() || expanded.starts_with(':') || expanded.starts_with("file:") {
        return Err(invalid(
            "`path` must be a file path, not empty, `:memory:` or a `file:` URI",
        ));
    }
    Ok(PathBuf::from(expanded))
}

/// The longest interval or timeout a setting takes: past it, deadlines
/// computed from it overflow, and `SQLite` takes its busy timeout as a 32-bit
/// count of milliseconds.
const MAX_MILLIS: u64 = 86_400_000;

/// The most events one sweep transaction removes: the sweep loads that many
/// rows under the write lock.
const MAX_SWEEP_BATCH: u32 = 100_000;

fn millis(value: &serde_json::Value, name: &str) -> Result<Duration, BackendError> {
    value
        .as_u64()
        .filter(|ms| (1..=MAX_MILLIS).contains(ms))
        .map(Duration::from_millis)
        .ok_or_else(|| {
            invalid(format!(
                "`{name}` must be a positive integer of at most {MAX_MILLIS} (one day)"
            ))
        })
}

fn positive(value: &serde_json::Value, name: &str) -> Result<u32, BackendError> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid(format!("`{name}` must be a positive integer")))
}

fn sweep(value: &serde_json::Value, mut parsed: Sweep) -> Result<Sweep, BackendError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("`sweep` must be { interval_ms, batch }"))?;
    for (name, value) in object {
        match name.as_str() {
            "interval_ms" => parsed.interval = millis(value, "sweep.interval_ms")?,
            "batch" => {
                parsed.batch = Some(positive(value, "sweep.batch")?)
                    .filter(|batch| *batch <= MAX_SWEEP_BATCH)
                    .ok_or_else(|| {
                        invalid(format!(
                            "`sweep.batch` must be a positive integer of at most {MAX_SWEEP_BATCH}"
                        ))
                    })?;
            }
            other => {
                return Err(invalid(format!(
                    "`sweep.{other}` is not a sqlite backend setting"
                )));
            }
        }
    }
    Ok(parsed)
}
