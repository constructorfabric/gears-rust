//! Configuration for the `TimescaleDB` usage-collector plugin.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;

const ONE_HOUR_SECS: u64 = 3_600;
const SEVEN_DAYS_SECS: u64 = 7 * 24 * 3_600;
const THIRTY_DAYS_SECS: u64 = 30 * 24 * 3_600;
const SEVEN_YEARS_SECS: u64 = 7 * 365 * 24 * 3_600;

fn default_pool_size_min() -> u32 {
    2
}

fn default_pool_size_max() -> u32 {
    16
}

fn default_retention_default() -> Duration {
    Duration::from_hours(8760)
}

fn default_idempotency_retention() -> Duration {
    Duration::from_secs(SEVEN_DAYS_SECS)
}

fn default_connection_timeout() -> Duration {
    Duration::from_secs(10)
}

fn deserialize_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    humantime::parse_duration(&s).map_err(serde::de::Error::custom)
}

/// Plugin configuration.
///
/// All parameters are static and require a plugin restart to change.
/// `database_url` must include `sslmode=require`, `sslmode=verify-ca`, or
/// `sslmode=verify-full`; plaintext connections are rejected.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimescaleDbConfig {
    /// `PostgreSQL` connection URL. Must include `sslmode=require`,
    /// `sslmode=verify-ca`, or `sslmode=verify-full`.
    #[serde(default)]
    pub database_url: String,

    /// Minimum connection pool size (1–64). Default: 2.
    #[serde(default = "default_pool_size_min")]
    pub pool_size_min: u32,

    /// Maximum connection pool size (1–128). Must be >= `pool_size_min`. Default: 16.
    #[serde(default = "default_pool_size_max")]
    pub pool_size_max: u32,

    /// Default data retention period (7 days – 7 years). Default: 365 days.
    #[serde(
        default = "default_retention_default",
        deserialize_with = "deserialize_duration"
    )]
    pub retention_default: Duration,

    /// Connection acquisition timeout (1s – 60s). Default: 10s.
    #[serde(
        default = "default_connection_timeout",
        deserialize_with = "deserialize_duration"
    )]
    pub connection_timeout: Duration,

    /// Retention period for `usage_idempotency_keys` rows (1h – 30 days). Default: 7 days.
    ///
    /// Distinct from `retention_default`, which controls the hypertable
    /// retention policy. Idempotency dedup windows are bounded by client retry
    /// horizons (typically hours to a few days); reusing the data-retention
    /// window here would grow `usage_idempotency_keys` linearly with ingest
    /// volume and slow the periodic `DELETE` over time.
    #[serde(
        default = "default_idempotency_retention",
        deserialize_with = "deserialize_duration"
    )]
    pub idempotency_retention: Duration,
}

impl fmt::Debug for TimescaleDbConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TimescaleDbConfig")
            .field("database_url", &"[redacted]")
            .field("pool_size_min", &self.pool_size_min)
            .field("pool_size_max", &self.pool_size_max)
            .field("retention_default", &self.retention_default)
            .field("connection_timeout", &self.connection_timeout)
            .field("idempotency_retention", &self.idempotency_retention)
            .finish()
    }
}

impl TimescaleDbConfig {
    /// Validates all configuration parameters.
    ///
    /// # Errors
    ///
    /// Returns an error if `database_url` is missing or its effective `sslmode`
    /// is not one of `require`, `verify-ca`, or `verify-full`, pool sizes are
    /// out of range, or timeouts are out of range.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.database_url.is_empty() {
            anyhow::bail!("database_url is required");
        }
        // Parse the query string to find the effective sslmode (last occurrence wins, matching
        // how the PostgreSQL driver resolves duplicate parameters).
        let sslmode = self.database_url.split_once('?').and_then(|(_, qs)| {
            qs.split('&')
                .rfind(|p| p.split_once('=').is_some_and(|(k, _)| k == "sslmode"))
                .and_then(|p| p.split_once('=').map(|(_, v)| v))
        });
        match sslmode {
            Some("require" | "verify-ca" | "verify-full") => {}
            _ => anyhow::bail!(
                "database_url must include sslmode=require, sslmode=verify-ca, or sslmode=verify-full for TLS enforcement"
            ),
        }
        if self.pool_size_min < 1 {
            anyhow::bail!("pool_size_min must be >= 1, got {}", self.pool_size_min);
        }
        if self.pool_size_min > 64 {
            anyhow::bail!("pool_size_min must be <= 64, got {}", self.pool_size_min);
        }
        if self.pool_size_max < self.pool_size_min {
            anyhow::bail!(
                "pool_size_max ({}) must be >= pool_size_min ({})",
                self.pool_size_max,
                self.pool_size_min
            );
        }
        if self.pool_size_max > 128 {
            anyhow::bail!("pool_size_max must be <= 128, got {}", self.pool_size_max);
        }
        let min_retention = Duration::from_secs(SEVEN_DAYS_SECS);
        let max_retention = Duration::from_secs(SEVEN_YEARS_SECS);
        if self.retention_default < min_retention || self.retention_default > max_retention {
            anyhow::bail!("retention_default must be between 7 days and 7 years");
        }
        if self.connection_timeout < Duration::from_secs(1)
            || self.connection_timeout > Duration::from_mins(1)
        {
            anyhow::bail!("connection_timeout must be between 1s and 60s");
        }
        let min_idem_retention = Duration::from_secs(ONE_HOUR_SECS);
        let max_idem_retention = Duration::from_secs(THIRTY_DAYS_SECS);
        if self.idempotency_retention < min_idem_retention
            || self.idempotency_retention > max_idem_retention
        {
            anyhow::bail!("idempotency_retention must be between 1 hour and 30 days");
        }
        Ok(())
    }
}

impl Default for TimescaleDbConfig {
    fn default() -> Self {
        Self {
            database_url: String::new(),
            pool_size_min: default_pool_size_min(),
            pool_size_max: default_pool_size_max(),
            retention_default: default_retention_default(),
            connection_timeout: default_connection_timeout(),
            idempotency_retention: default_idempotency_retention(),
        }
    }
}
