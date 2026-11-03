//! A `PostgreSQL` registry entry's settings (`docs/DESIGN.md` §5).
//!
//! Read by hand rather than through serde, so an error names the setting and
//! the rule it breaks and never quotes a value: `url` carries a password.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use event_broker_sdk::backend::BackendError;
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

/// Who sends an append's head notification (`docs/DESIGN.md` §2.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadNotify {
    /// The append, in its transaction: delivered exactly when the events
    /// are visible, at the price of notifying commits running one at a time.
    InTransaction,
    /// A worker of its own, after the commit, for many appends at once: a
    /// crash in between loses the notification.
    AfterCommit,
}

/// The background maintenance (`docs/DESIGN.md` §4.9): how often the rescan
/// runs, how many workers each job has, and when a postponed partition is
/// reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaintenanceSettings {
    /// Between two rescans of every partition.
    pub rescan_interval: Duration,
    /// Rescans per cross-check of the segment rows with the catalog.
    pub crosscheck_every: u32,
    /// Extender workers, one pooled connection each.
    pub extender: u32,
    /// Sealer workers, one pooled connection each.
    pub sealer: u32,
    /// Retention workers, one pooled connection each.
    pub retention: u32,
    /// Reconcile workers, one pooled connection each.
    pub reconcile: u32,
    /// Detacher workers, one dedicated connection each, outside the pool.
    pub detacher: u32,
    /// Postponements in a row after which a partition is reported.
    pub warn_after: u32,
    /// Postponements in a row after which a partition is stuck.
    pub stuck_after: u32,
    /// How long a partition may keep being postponed before it is stuck.
    pub stuck_age: Duration,
}

impl Default for MaintenanceSettings {
    fn default() -> Self {
        Self {
            rescan_interval: Duration::from_mins(1),
            crosscheck_every: 10,
            extender: 2,
            sealer: 1,
            retention: 1,
            reconcile: 1,
            detacher: 1,
            warn_after: 5,
            stuck_after: 20,
            stuck_age: Duration::from_mins(5),
        }
    }
}

impl MaintenanceSettings {
    /// The size of the maintenance's own pool: one connection per pooled
    /// worker, and the rescan's.
    pub fn pooled_connections(&self) -> u32 {
        self.extender
            .saturating_add(self.sealer)
            .saturating_add(self.retention)
            .saturating_add(self.reconcile)
            .saturating_add(1)
    }
}

/// The plugin's two pools, by what uses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pools {
    /// Every operation the gear calls, whoever calls it.
    pub main: u32,
    /// The maintenance workers but the detacher; only in a process that
    /// runs them.
    pub background: u32,
}

/// One registry entry's settings, defaults applied.
#[derive(Clone, PartialEq, Eq)]
pub struct PostgresSettings {
    /// The connection URL, `${VAR}` expanded.
    pub url: String,
    /// The schema holding every object of this plugin.
    pub schema: String,
    /// The sizes of the plugin's pools.
    pub pools: Pools,
    /// How long an operation waits for a pooled connection.
    pub pool_acquire_timeout: Duration,
    /// How the next segment of a partition is sized.
    pub segment: SegmentRule,
    /// Who sends an append's head notification.
    pub head_notify: HeadNotify,
    /// The background maintenance.
    pub maintenance: MaintenanceSettings,
}

impl fmt::Debug for PostgresSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresSettings")
            .field("url", &"<redacted>")
            .field("schema", &self.schema)
            .field("pools", &self.pools)
            .field("pool_acquire_timeout", &self.pool_acquire_timeout)
            .field("segment", &self.segment)
            .field("head_notify", &self.head_notify)
            .field("maintenance", &self.maintenance)
            .finish()
    }
}

const MAX_IDENTIFIER: usize = 63;

impl PostgresSettings {
    /// Reads an entry's settings; an unknown setting or a broken rule is
    /// `InvalidConfig`.
    pub fn parse(
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, BackendError> {
        let mut url = None;
        let mut main = None;
        let mut background = None;
        let mut parsed = Self {
            url: String::new(),
            schema: "evbk".to_owned(),
            pools: Pools {
                main: 10,
                background: 0,
            },
            pool_acquire_timeout: Duration::from_secs(5),
            segment: SegmentRule::Events(1_000_000),
            head_notify: HeadNotify::InTransaction,
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
                "schema" => {
                    let schema = value
                        .as_str()
                        .filter(|schema| is_identifier(schema))
                        .ok_or_else(|| {
                            invalid("`schema` must be a bare identifier ([a-z_][a-z0-9_]*) of at most 63 bytes")
                        })?;
                    schema.clone_into(&mut parsed.schema);
                }
                "pools" => (main, background) = pools(value)?,
                "pool_acquire_timeout_ms" => {
                    parsed.pool_acquire_timeout = millis(value, "pool_acquire_timeout_ms")?;
                }
                "segment" => parsed.segment = segment_rule(value)?,
                "head_notify" => {
                    parsed.head_notify = match value.as_str() {
                        Some("in_transaction") => HeadNotify::InTransaction,
                        Some("after_commit") => HeadNotify::AfterCommit,
                        _ => {
                            return Err(invalid(
                                "`head_notify` must be \"in_transaction\" or \"after_commit\"",
                            ));
                        }
                    };
                }
                "maintenance" => parsed.maintenance = maintenance(value)?,
                "pgbouncer_transaction_mode" => {
                    let on = value
                        .as_bool()
                        .ok_or_else(|| invalid("`pgbouncer_transaction_mode` must be a boolean"))?;
                    if on {
                        return Err(invalid(
                            "`pgbouncer_transaction_mode` is not supported: LISTEN needs a session-pooled connection",
                        ));
                    }
                }
                other => {
                    return Err(invalid(format!(
                        "`{other}` is not a postgres backend setting"
                    )));
                }
            }
        }
        parsed.url = url.ok_or_else(|| invalid("`url` is required"))?;
        // A worker that waits for a background connection times out, and that
        // reads as the database being down.
        let workers = parsed.maintenance.pooled_connections();
        parsed.pools.main = main.unwrap_or(parsed.pools.main);
        parsed.pools.background = background.unwrap_or(workers);
        if parsed.pools.background < workers {
            return Err(invalid(
                "`pools.background` must be at least the maintenance's pooled workers: \
                 extender + sealer + retention + reconcile concurrency, plus one for the rescan",
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

fn positive(value: &serde_json::Value, name: &str) -> Result<u32, BackendError> {
    value
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| invalid(format!("`{name}` must be a positive integer")))
}

/// `(main, background)`, each `None` when not given.
fn pools(value: &serde_json::Value) -> Result<(Option<u32>, Option<u32>), BackendError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("`pools` must be { main, background }"))?;
    let (mut main, mut background) = (None, None);
    for (name, value) in object {
        match name.as_str() {
            "main" => main = Some(positive(value, "pools.main")?),
            "background" => background = Some(positive(value, "pools.background")?),
            other => {
                return Err(invalid(format!(
                    "`pools.{other}` is not a postgres backend setting"
                )));
            }
        }
    }
    Ok((main, background))
}

fn maintenance(value: &serde_json::Value) -> Result<MaintenanceSettings, BackendError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("`maintenance` must be an object"))?;
    let mut parsed = MaintenanceSettings::default();
    for (name, value) in object {
        match name.as_str() {
            "rescan_interval_ms" => {
                parsed.rescan_interval = millis(value, "maintenance.rescan_interval_ms")?;
            }
            "crosscheck_every" => {
                parsed.crosscheck_every = positive(value, "maintenance.crosscheck_every")?;
            }
            "extender" => parsed.extender = concurrency(value, "extender")?,
            "sealer" => parsed.sealer = concurrency(value, "sealer")?,
            "retention" => parsed.retention = concurrency(value, "retention")?,
            "reconcile" => parsed.reconcile = concurrency(value, "reconcile")?,
            "detacher" => parsed.detacher = concurrency(value, "detacher")?,
            "postponed" => postponed(value, &mut parsed)?,
            other => {
                return Err(invalid(format!(
                    "`maintenance.{other}` is not a postgres backend setting"
                )));
            }
        }
    }
    Ok(parsed)
}

fn concurrency(value: &serde_json::Value, job: &str) -> Result<u32, BackendError> {
    value
        .as_object()
        .filter(|o| o.len() == 1)
        .and_then(|o| o.get("concurrency"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| {
            invalid(format!(
                "`maintenance.{job}` must be {{ concurrency: <positive integer> }}"
            ))
        })
}

fn postponed(
    value: &serde_json::Value,
    parsed: &mut MaintenanceSettings,
) -> Result<(), BackendError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("`maintenance.postponed` must be an object"))?;
    for (name, value) in object {
        match name.as_str() {
            "warn_after" => {
                parsed.warn_after = positive(value, "maintenance.postponed.warn_after")?;
            }
            "stuck_after" => {
                parsed.stuck_after = positive(value, "maintenance.postponed.stuck_after")?;
            }
            "stuck_age_ms" => {
                parsed.stuck_age = millis(value, "maintenance.postponed.stuck_age_ms")?;
            }
            other => {
                return Err(invalid(format!(
                    "`maintenance.postponed.{other}` is not a postgres backend setting"
                )));
            }
        }
    }
    Ok(())
}

fn segment_rule(value: &serde_json::Value) -> Result<SegmentRule, BackendError> {
    let rule = || {
        invalid(
            "`segment` must be { events: <positive integer> } or { period: <ISO 8601 duration> }",
        )
    };
    let object = value
        .as_object()
        .filter(|o| o.len() == 1)
        .ok_or_else(rule)?;
    if let Some(events) = object.get("events") {
        return events
            .as_u64()
            .filter(|n| *n > 1)
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
