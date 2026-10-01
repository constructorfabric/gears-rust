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

#[derive(Clone, PartialEq, Eq)]
pub struct PostgresSettings {
    /// The connection URL, `${VAR}` expanded.
    pub url: String,
    pub schema: String,
    pub pool_max_size: u32,
    pub pool_acquire_timeout: Duration,
    pub maintenance_interval: Duration,
    pub segment: SegmentRule,
}

impl fmt::Debug for PostgresSettings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresSettings")
            .field("url", &"<redacted>")
            .field("schema", &self.schema)
            .field("pool_max_size", &self.pool_max_size)
            .field("pool_acquire_timeout", &self.pool_acquire_timeout)
            .field("maintenance_interval", &self.maintenance_interval)
            .field("segment", &self.segment)
            .finish()
    }
}

const MAX_IDENTIFIER: usize = 63;

impl PostgresSettings {
    pub fn parse(
        settings: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Self, BackendError> {
        let mut url = None;
        let mut parsed = Self {
            url: String::new(),
            schema: "evbk".to_owned(),
            pool_max_size: 10,
            pool_acquire_timeout: Duration::from_secs(5),
            maintenance_interval: Duration::from_secs(10),
            segment: SegmentRule::Events(1_000_000),
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
                "pool_max_size" => {
                    parsed.pool_max_size = value
                        .as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .filter(|n| *n > 0)
                        .ok_or_else(|| invalid("`pool_max_size` must be a positive integer"))?;
                }
                "pool_acquire_timeout_ms" => {
                    parsed.pool_acquire_timeout = millis(value, "pool_acquire_timeout_ms")?;
                }
                "maintenance_interval_ms" => {
                    parsed.maintenance_interval = millis(value, "maintenance_interval_ms")?;
                }
                "segment" => parsed.segment = segment_rule(value)?,
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
