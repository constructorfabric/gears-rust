//! Configuration for the Postgres cluster plugin (DESIGN.md §7).
//!
//! Two config types exist because the combined cache+lock plugin and the
//! standalone lock-only provider (DESIGN.md §3.5) need different field sets:
//! [`PostgresClusterConfig`] carries the cache-only fields
//! (`cache_reaper_interval_ms`, `sd_poll_interval_ms`) that
//! [`PostgresLockConfig`] omits. Defaults for the fields both types share are
//! centralized in this module's `default_*` functions so the two types cannot
//! drift (DESIGN.md §7 calls this out explicitly).

use std::fmt;
use std::time::Duration;

use cluster_sdk::ClusterError;
use serde::Deserialize;

/// The masked stand-in rendered for `connection_string` in `Debug` output, so a
/// `{:?}` of a config (in a log line or a panic message) never leaks the DB
/// password the expanded DSN embeds (PGR-M9). The two config types hand-write
/// `Debug` rather than `#[derive]`ing it for this reason.
const REDACTED_DSN: &str = "<redacted>";

/// Default pool size (write pool). DESIGN.md §7.
pub fn default_pool_size() -> u32 {
    5
}

/// Default pool acquire timeout, in milliseconds. DESIGN.md §7.
pub fn default_acquire_timeout() -> u64 {
    5_000
}

/// Default schema for plugin tables. DESIGN.md §7.
pub fn default_schema() -> String {
    "public".to_owned()
}

/// Default TTL reaper interval for `cluster_cache`, in milliseconds. DESIGN.md §7.
pub fn default_reaper_interval() -> u64 {
    10_000
}

/// Default TTL reaper interval for `cluster_lock`, in milliseconds. DESIGN.md §7.
pub fn default_lock_reaper_interval() -> u64 {
    5_000
}

/// Default polling interval for the service-discovery `PollingPrefixWatch`, in
/// milliseconds. DESIGN.md §7.
pub fn default_sd_poll_interval() -> u64 {
    5_000
}

/// Default lock-name-cardinality WARN threshold. DESIGN.md §7 / §8 / §11.
pub fn default_lock_name_cardinality_warn_threshold() -> u32 {
    1_000
}

/// Operator hint for replication topology (DESIGN.md §3.6).
///
/// If omitted from config, the plugin detects the effective mode at startup
/// via `SHOW synchronous_standby_names` (empty result → `Async`). `Async` logs
/// `cluster.provider.replication_async` (WARN, once) per ADR-009's safety
/// table but never fails startup — see DESIGN.md §3.6 for the full rationale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationMode {
    /// No synchronous standby configured — the common default. Leader-election
    /// and lock claims are not failover-safe under this topology.
    Async,
    /// A synchronous standby is configured. Does not upgrade this plugin's
    /// declared `consistency()`/`*Features` — it only suppresses the WARN
    /// (DESIGN.md §3.6).
    Sync,
}

/// Configuration for the combined `PostgresClusterPlugin` (cache + lock,
/// DESIGN.md §3.2).
#[derive(Clone, Deserialize, toolkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct PostgresClusterConfig {
    /// sqlx connection string. Supports `${VAR}` / `${VAR:-default}` env-var
    /// expansion (e.g. `postgres://user:${DB_PASSWORD}@db:5432/gears`) via
    /// `toolkit_utils::var_expand`, the same mechanism `libs/toolkit-db` uses
    /// for DB passwords/DSNs. A credstore-backed (`secret_ref`) resolution
    /// path is deferred to a future iteration (DESIGN.md §12).
    #[expand_vars]
    pub connection_string: String,

    /// Maximum pool size (write pool). Default: 5.
    #[serde(default = "default_pool_size")]
    pub pool_max_size: u32,

    /// Pool acquire timeout, in milliseconds. Default: 5000.
    #[serde(default = "default_acquire_timeout")]
    pub pool_acquire_timeout_ms: u64,

    /// Schema for plugin tables. Default: `"public"`.
    #[serde(default = "default_schema")]
    pub schema: String,

    /// TTL reaper interval for `cluster_cache`, in milliseconds. Default: 10000.
    #[serde(default = "default_reaper_interval")]
    pub cache_reaper_interval_ms: u64,

    /// TTL reaper interval for `cluster_lock`, in milliseconds. Default: 5000.
    #[serde(default = "default_lock_reaper_interval")]
    pub lock_reaper_interval_ms: u64,

    /// Polling interval for the service-discovery `PollingPrefixWatch`, in
    /// milliseconds. Default: 5000.
    #[serde(default = "default_sd_poll_interval")]
    pub sd_poll_interval_ms: u64,

    /// Set to `true` to get an `InvalidConfig` error at startup rather than
    /// silent mis-behaviour if the connection string points to a `PgBouncer` in
    /// transaction mode. Default: `false`.
    #[serde(default)]
    pub pgbouncer_transaction_mode: bool,

    /// Distinct concurrently-held lock-name count past which the lock reaper
    /// logs `cluster.lock.name_cardinality_high` (WARN) and the
    /// `cluster_postgres_lock_active_names` gauge should be alerted on.
    /// Default: 1000 (DESIGN.md §5.1/§8/§11).
    #[serde(default = "default_lock_name_cardinality_warn_threshold")]
    pub lock_name_cardinality_warn_threshold: u32,

    /// Operator hint for replication topology. If omitted, detected at
    /// startup (DESIGN.md §3.6).
    #[serde(default)]
    pub replication_mode: Option<ReplicationMode>,
}

impl fmt::Debug for PostgresClusterConfig {
    /// Hand-written so `connection_string` (which embeds the DB password after
    /// `expand_vars`) is masked — see [`REDACTED_DSN`] (PGR-M9).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresClusterConfig")
            .field("connection_string", &REDACTED_DSN)
            .field("pool_max_size", &self.pool_max_size)
            .field("pool_acquire_timeout_ms", &self.pool_acquire_timeout_ms)
            .field("schema", &self.schema)
            .field("cache_reaper_interval_ms", &self.cache_reaper_interval_ms)
            .field("lock_reaper_interval_ms", &self.lock_reaper_interval_ms)
            .field("sd_poll_interval_ms", &self.sd_poll_interval_ms)
            .field(
                "pgbouncer_transaction_mode",
                &self.pgbouncer_transaction_mode,
            )
            .field(
                "lock_name_cardinality_warn_threshold",
                &self.lock_name_cardinality_warn_threshold,
            )
            .field("replication_mode", &self.replication_mode)
            .finish()
    }
}

/// Rejects a reaper/poll interval of `0`, which would panic
/// [`tokio::time::interval`] ("delay is zero") the moment the reaper task
/// starts (PGR-E2). `field` names the offending config key in the error.
fn reject_zero_interval(value: u64, field: &str) -> Result<(), ClusterError> {
    if value == 0 {
        return Err(ClusterError::InvalidConfig {
            reason: format!("{field} must be greater than zero"),
        });
    }
    Ok(())
}

impl PostgresClusterConfig {
    /// Validates config values that can only fail at startup — the schema
    /// identifier (PGR-L4) and the non-zero reaper/poll intervals (PGR-E2) —
    /// before any pool or reaper is constructed. Called at the top of
    /// `build_and_start`.
    ///
    /// # Errors
    /// [`ClusterError::InvalidConfig`] for an unsafe `schema` or a zero
    /// `cache_reaper_interval_ms` / `lock_reaper_interval_ms` /
    /// `sd_poll_interval_ms`.
    pub fn validate(&self) -> Result<(), ClusterError> {
        crate::pg_setup::validate_schema(&self.schema)?;
        reject_zero_interval(self.cache_reaper_interval_ms, "cache_reaper_interval_ms")?;
        reject_zero_interval(self.lock_reaper_interval_ms, "lock_reaper_interval_ms")?;
        reject_zero_interval(self.sd_poll_interval_ms, "sd_poll_interval_ms")?;
        Ok(())
    }

    /// The pool acquire timeout as a [`Duration`].
    #[must_use]
    pub fn pool_acquire_timeout(&self) -> Duration {
        Duration::from_millis(self.pool_acquire_timeout_ms)
    }

    /// The cache TTL reaper interval as a [`Duration`].
    #[must_use]
    pub fn cache_reaper_interval(&self) -> Duration {
        Duration::from_millis(self.cache_reaper_interval_ms)
    }

    /// The lock TTL reaper interval as a [`Duration`].
    #[must_use]
    pub fn lock_reaper_interval(&self) -> Duration {
        Duration::from_millis(self.lock_reaper_interval_ms)
    }

    /// The service-discovery polling interval as a [`Duration`].
    #[must_use]
    pub fn sd_poll_interval(&self) -> Duration {
        Duration::from_millis(self.sd_poll_interval_ms)
    }
}

/// Configuration for the standalone `PostgresLockPlugin` (DESIGN.md §3.5).
///
/// A separate, smaller config type — it only carries the fields the lock
/// primitive actually uses, not the cache-only ones
/// (`cache_reaper_interval_ms`, `sd_poll_interval_ms`).
#[derive(Clone, Deserialize, toolkit_macros::ExpandVars)]
#[serde(deny_unknown_fields)]
pub struct PostgresLockConfig {
    /// See [`PostgresClusterConfig::connection_string`].
    #[expand_vars]
    pub connection_string: String,

    /// See [`PostgresClusterConfig::pool_max_size`].
    #[serde(default = "default_pool_size")]
    pub pool_max_size: u32,

    /// See [`PostgresClusterConfig::pool_acquire_timeout_ms`].
    #[serde(default = "default_acquire_timeout")]
    pub pool_acquire_timeout_ms: u64,

    /// See [`PostgresClusterConfig::schema`].
    #[serde(default = "default_schema")]
    pub schema: String,

    /// See [`PostgresClusterConfig::lock_reaper_interval_ms`].
    #[serde(default = "default_lock_reaper_interval")]
    pub lock_reaper_interval_ms: u64,

    /// See [`PostgresClusterConfig::pgbouncer_transaction_mode`].
    #[serde(default)]
    pub pgbouncer_transaction_mode: bool,

    /// See [`PostgresClusterConfig::lock_name_cardinality_warn_threshold`].
    #[serde(default = "default_lock_name_cardinality_warn_threshold")]
    pub lock_name_cardinality_warn_threshold: u32,

    /// See [`PostgresClusterConfig::replication_mode`].
    #[serde(default)]
    pub replication_mode: Option<ReplicationMode>,
}

impl fmt::Debug for PostgresLockConfig {
    /// Hand-written so `connection_string` is masked — see [`REDACTED_DSN`]
    /// (PGR-M9).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostgresLockConfig")
            .field("connection_string", &REDACTED_DSN)
            .field("pool_max_size", &self.pool_max_size)
            .field("pool_acquire_timeout_ms", &self.pool_acquire_timeout_ms)
            .field("schema", &self.schema)
            .field("lock_reaper_interval_ms", &self.lock_reaper_interval_ms)
            .field(
                "pgbouncer_transaction_mode",
                &self.pgbouncer_transaction_mode,
            )
            .field(
                "lock_name_cardinality_warn_threshold",
                &self.lock_name_cardinality_warn_threshold,
            )
            .field("replication_mode", &self.replication_mode)
            .finish()
    }
}

impl PostgresLockConfig {
    /// Validates the schema identifier (PGR-L4) and the non-zero lock reaper
    /// interval (PGR-E2) before any pool or reaper is constructed. Called at the
    /// top of `build_and_start`.
    ///
    /// # Errors
    /// [`ClusterError::InvalidConfig`] for an unsafe `schema` or a zero
    /// `lock_reaper_interval_ms`.
    pub fn validate(&self) -> Result<(), ClusterError> {
        crate::pg_setup::validate_schema(&self.schema)?;
        reject_zero_interval(self.lock_reaper_interval_ms, "lock_reaper_interval_ms")?;
        Ok(())
    }

    /// The pool acquire timeout as a [`Duration`].
    #[must_use]
    pub fn pool_acquire_timeout(&self) -> Duration {
        Duration::from_millis(self.pool_acquire_timeout_ms)
    }

    /// The lock TTL reaper interval as a [`Duration`].
    #[must_use]
    pub fn lock_reaper_interval(&self) -> Duration {
        Duration::from_millis(self.lock_reaper_interval_ms)
    }
}

// Layer-1 unit tests (TESTING.md §2, config.rs row). Pure serde/expansion — no
// container. `pgbouncer_transaction_mode: true` rejection is builder-level (a
// `build_and_start` concern that needs a pool), covered by the Layer-3 suite,
// not here. Out-of-line (DE1101: an inline test block over 100 lines must live
// in a separate `*_tests.rs` file), mirroring `lock_tests.rs` / `watch_tests.rs`.
#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
