//! A `MySQL`/`MariaDB` registry entry's settings (`docs/DESIGN.md` §5).
//!
//! Read by hand rather than through serde, so an error names the setting and
//! the rule it breaks and never quotes a value: `url` carries a password.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use event_broker_sdk::backend::BackendError;
use sqlx::mysql::MySqlConnectOptions;
use toolkit_utils::iso8601_duration::Iso8601Duration;
use toolkit_utils::var_expand::expand_env_vars;

/// How the next segment of a partition is sized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentRule {
    /// A fixed number of sequences per segment.
    Events(u64),
    /// About one period of traffic per segment, from the observed rate.
    Period(Duration),
}

/// The background maintenance (`docs/DESIGN.md` §4.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintenanceSettings {
    /// Between two rescans of every partition.
    pub rescan_interval: Duration,
    /// Rescans per cross-check of the segment rows with the server's
    /// partitions.
    pub crosscheck_every: u32,
    /// The `lock_wait_timeout` of the first attempt at a partition DDL; each
    /// retry of the same partition waits one second longer, up to four times
    /// this.
    pub ddl_lock_wait: Duration,
}

impl Default for MaintenanceSettings {
    fn default() -> Self {
        Self {
            rescan_interval: Duration::from_mins(1),
            crosscheck_every: 10,
            ddl_lock_wait: Duration::from_secs(1),
        }
    }
}

/// The plugin's two pools, by what uses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pools {
    /// Every operation the gear calls, and the head poll.
    pub main: u32,
    /// The maintenance worker and the rescan; only in a process that runs
    /// them.
    pub background: u32,
}

/// The least the background pool holds: the maintenance worker's connection
/// and the rescan's.
pub const MIN_BACKGROUND: u32 = 2;

/// One registry entry's settings, defaults applied.
#[derive(Clone, PartialEq, Eq)]
pub struct MysqlSettings {
    /// The connection URL, `${VAR}` expanded.
    pub url: String,
    /// The database holding every object of this plugin; created at start
    /// when missing.
    pub database: String,
    /// The sizes of the plugin's pools.
    pub pools: Pools,
    /// How long an operation waits for a pooled connection.
    pub pool_acquire_timeout: Duration,
    /// How the next segment of a partition is sized.
    pub segment: SegmentRule,
    /// How often heads are polled for watches, and how often a waiting read
    /// looks again for appends made by other instances.
    pub poll_interval: Duration,
    /// Whether a server that does not flush every commit keeps the backend
    /// `Unavailable`, rather than only warning.
    pub require_durable_server: bool,
    /// The background maintenance.
    pub maintenance: MaintenanceSettings,
}

impl fmt::Debug for MysqlSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MysqlSettings")
            .field("url", &"<redacted>")
            .field("database", &self.database)
            .field("pools", &self.pools)
            .field("pool_acquire_timeout", &self.pool_acquire_timeout)
            .field("segment", &self.segment)
            .field("poll_interval", &self.poll_interval)
            .field("require_durable_server", &self.require_durable_server)
            .field("maintenance", &self.maintenance)
            .finish()
    }
}

/// `MySQL`'s and `MariaDB`'s identifier limit.
const MAX_IDENTIFIER: usize = 64;

impl MysqlSettings {
    /// Reads an entry's settings; an unknown setting or a broken rule is
    /// `InvalidConfig`.
    pub fn parse(
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, BackendError> {
        let mut url = None;
        let mut parsed = Self {
            url: String::new(),
            database: "evbk".to_owned(),
            pools: Pools {
                main: 10,
                background: MIN_BACKGROUND,
            },
            pool_acquire_timeout: Duration::from_secs(5),
            segment: SegmentRule::Events(1_000_000),
            poll_interval: Duration::from_millis(500),
            require_durable_server: true,
            maintenance: MaintenanceSettings::default(),
        };
        for (name, value) in settings {
            match name.as_str() {
                "url" => {
                    let raw = value
                        .as_str()
                        .ok_or_else(|| invalid("`url` must be a string"))?;
                    url = Some(expand_env_vars(raw).map_err(|_| {
                        invalid("`url` names an environment variable that is not set")
                    })?);
                }
                "database" => {
                    let database = value
                        .as_str()
                        .filter(|database| is_identifier(database))
                        .ok_or_else(|| {
                            invalid("`database` must be a bare identifier ([a-z_][a-z0-9_]*) of at most 64 bytes")
                        })?;
                    database.clone_into(&mut parsed.database);
                }
                "pools" => pools(value, &mut parsed.pools)?,
                "pool_acquire_timeout_ms" => {
                    parsed.pool_acquire_timeout = millis(value, "pool_acquire_timeout_ms")?;
                }
                "segment" => parsed.segment = segment_rule(value)?,
                "poll_interval_ms" => {
                    parsed.poll_interval =
                        bounded_millis(value, "poll_interval_ms", POLL_INTERVAL)?;
                }
                "require_durable_server" => {
                    parsed.require_durable_server = value
                        .as_bool()
                        .ok_or_else(|| invalid("`require_durable_server` must be a boolean"))?;
                }
                "maintenance" => parsed.maintenance = maintenance(value)?,
                other => {
                    return Err(invalid(format!("`{other}` is not a mysql backend setting")));
                }
            }
        }
        parsed.url = url.ok_or_else(|| invalid("`url` is required"))?;
        // Parsed again at start; checked here, so a url that can never
        // connect fails the build instead of keeping the backend
        // `Unavailable` for good.
        let scheme_known = ["mysql://", "mariadb://"]
            .iter()
            .any(|scheme| parsed.url.starts_with(scheme));
        if !scheme_known || MySqlConnectOptions::from_str(&parsed.url).is_err() {
            return Err(invalid("`url` must be a mysql:// url the driver accepts"));
        }
        if parsed.pools.background < MIN_BACKGROUND {
            return Err(invalid(
                "`pools.background` must be at least 2: the maintenance worker and the rescan",
            ));
        }
        Ok(parsed)
    }
}

fn invalid(detail: impl fmt::Display) -> BackendError {
    BackendError::invalid_config(detail)
}

fn millis(value: &serde_json::Value, name: &str) -> Result<Duration, BackendError> {
    value
        .as_u64()
        .filter(|ms| *ms > 0)
        .map(Duration::from_millis)
        .ok_or_else(|| invalid(format!("`{name}` must be a positive integer")))
}

/// The range of `poll_interval_ms`, in milliseconds: below it the head poll
/// is a busy loop, above it a watch lags by more than a minute.
const POLL_INTERVAL: (u64, u64) = (10, 60_000);

/// The range of `maintenance.rescan_interval_ms`, in milliseconds: below it
/// the rescan reads every row nonstop, above it an age bound or a crash
/// waits more than a day to be acted on.
const RESCAN_INTERVAL: (u64, u64) = (100, 86_400_000);

/// The smallest `segment.events`: one full append, so a segment is never
/// smaller than what one append adds and a table's 8192-partition limit is
/// not reached by a few appends.
#[allow(
    clippy::cast_possible_truncation,
    reason = "a thousand fits any integer type"
)]
const MIN_SEGMENT_EVENTS: u64 = crate::append::MAX_EVENTS_PER_APPEND as u64;

fn bounded_millis(
    value: &serde_json::Value,
    name: &str,
    (low, high): (u64, u64),
) -> Result<Duration, BackendError> {
    value
        .as_u64()
        .filter(|ms| (low..=high).contains(ms))
        .map(Duration::from_millis)
        .ok_or_else(|| invalid(format!("`{name}` must be an integer from {low} to {high}")))
}

fn positive(value: &serde_json::Value, name: &str) -> Result<u32, BackendError> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid(format!("`{name}` must be a positive integer")))
}

fn pools(value: &serde_json::Value, parsed: &mut Pools) -> Result<(), BackendError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("`pools` must be { main, background }"))?;
    for (name, value) in object {
        match name.as_str() {
            "main" => parsed.main = positive(value, "pools.main")?,
            "background" => parsed.background = positive(value, "pools.background")?,
            other => {
                return Err(invalid(format!(
                    "`pools.{other}` is not a mysql backend setting"
                )));
            }
        }
    }
    Ok(())
}

fn maintenance(value: &serde_json::Value) -> Result<MaintenanceSettings, BackendError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("`maintenance` must be an object"))?;
    let mut parsed = MaintenanceSettings::default();
    for (name, value) in object {
        match name.as_str() {
            "rescan_interval_ms" => {
                parsed.rescan_interval =
                    bounded_millis(value, "maintenance.rescan_interval_ms", RESCAN_INTERVAL)?;
            }
            "crosscheck_every" => {
                parsed.crosscheck_every = positive(value, "maintenance.crosscheck_every")?;
            }
            "ddl_lock_wait_s" => {
                let secs = value
                    .as_u64()
                    .filter(|s| (1..=60).contains(s))
                    .ok_or_else(|| {
                        invalid("`maintenance.ddl_lock_wait_s` must be an integer from 1 to 60")
                    })?;
                parsed.ddl_lock_wait = Duration::from_secs(secs);
            }
            other => {
                return Err(invalid(format!(
                    "`maintenance.{other}` is not a mysql backend setting"
                )));
            }
        }
    }
    Ok(parsed)
}

fn segment_rule(value: &serde_json::Value) -> Result<SegmentRule, BackendError> {
    let rule = || {
        invalid(
            "`segment` must be { events: <integer of at least 1000> } or { period: <ISO 8601 duration> }",
        )
    };
    let object = value
        .as_object()
        .filter(|o| o.len() == 1)
        .ok_or_else(rule)?;
    if let Some(events) = object.get("events") {
        return events
            .as_u64()
            .filter(|n| *n >= MIN_SEGMENT_EVENTS)
            .map(SegmentRule::Events)
            .ok_or_else(rule);
    }
    let period = object
        .get("period")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(rule)?;
    Iso8601Duration::from_str(period)
        .ok()
        .map(Iso8601Duration::as_duration)
        .filter(|d| !d.is_zero())
        .map(SegmentRule::Period)
        .ok_or_else(rule)
}

/// A bare, lower-case SQL identifier: interpolated unquoted.
pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= MAX_IDENTIFIER
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
