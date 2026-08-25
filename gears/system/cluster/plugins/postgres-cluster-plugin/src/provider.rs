//! The [`ClusterCacheProvider`] and [`ClusterLockProvider`] implementations
//! for the Postgres backend (DESIGN.md §1.1, §3.5).
//!
//! Two independent provider impls, not one: `PostgresLockProvider` lets the
//! wiring crate route `lock: { provider: postgres }` even when `cache` in the
//! same profile is bound elsewhere (or omitted) — see DESIGN.md §3.5 for why
//! this is *always standalone, never shared* with a co-located cache pool.

use std::sync::Arc;

use async_trait::async_trait;
use cluster_sdk::{
    ClusterCacheBackend, ClusterCacheProvider, ClusterError, ClusterLockProvider,
    DistributedLockBackend, StopHook,
};
use toolkit::var_expand::ExpandVars;

use crate::config::{PostgresClusterConfig, PostgresLockConfig};
use crate::lock::PostgresLockPlugin;
use crate::plugin::PostgresClusterPlugin;

/// The operator config `provider` name that selects the Postgres backend, for
/// both the `cache` and `lock` primitive bindings.
pub const PROVIDER_NAME: &str = "postgres";

/// Deserializes `options` into `T` and applies `#[derive(ExpandVars)]`
/// expansion, mapping both failure modes to
/// [`ClusterError::InvalidConfig`] — a malformed option or a missing
/// referenced env var is an operator error, not a silent fallback (DESIGN.md
/// §7 / `docs/pr-review` TOOLKIT-DB config conventions).
fn deserialize_and_expand<T>(
    options: &serde_json::Map<String, serde_json::Value>,
) -> Result<T, ClusterError>
where
    T: serde::de::DeserializeOwned + ExpandVars,
{
    let mut config: T = serde_json::from_value(serde_json::Value::Object(options.clone()))
        .map_err(|err| ClusterError::InvalidConfig {
            reason: format!("postgres: invalid options: {err}"),
        })?;
    config
        .expand_vars()
        .map_err(|err| ClusterError::InvalidConfig {
            reason: format!("postgres: `connection_string` env-var expansion failed: {err}"),
        })?;
    Ok(config)
}

/// Builds the combined Postgres cache backend from operator config.
pub struct PostgresCacheProvider;

#[async_trait]
impl ClusterCacheProvider for PostgresCacheProvider {
    fn provider(&self) -> &'static str {
        PROVIDER_NAME
    }

    async fn build_cache(
        &self,
        options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(Arc<dyn ClusterCacheBackend>, StopHook), ClusterError> {
        let config: PostgresClusterConfig = deserialize_and_expand(options)?;
        let handle = PostgresClusterPlugin::builder(config)
            .build_and_start()
            .await?;
        let cache = handle.cache();
        let stop: StopHook = Box::new(move || Box::pin(async move { handle.stop().await }));
        Ok((cache, stop))
    }
}

/// Builds the standalone Postgres lock backend from operator config
/// (DESIGN.md §3.5). Never receives or depends on a cache backend argument —
/// matches the SDK provider trait's "non-cache providers do not receive the
/// cache backend" contract.
pub struct PostgresLockProvider;

#[async_trait]
impl ClusterLockProvider for PostgresLockProvider {
    fn provider(&self) -> &'static str {
        PROVIDER_NAME
    }

    async fn build_lock(
        &self,
        options: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(Arc<dyn DistributedLockBackend>, StopHook), ClusterError> {
        let config: PostgresLockConfig = deserialize_and_expand(options)?;
        let handle = PostgresLockPlugin::builder(config)
            .build_and_start()
            .await?;
        let lock = handle.lock();
        let stop: StopHook = Box::new(move || Box::pin(async move { handle.stop().await }));
        Ok((lock, stop))
    }
}

// Layer-1 unit tests (TESTING.md §2, provider.rs row). The `build_cache` /
// `build_lock` cases below fail at config deserialization / env-var expansion —
// *before* any pool is opened — so they need no container.
#[cfg(test)]
mod provider_tests {
    use super::*;
    use serde_json::json;

    fn options(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        match value {
            serde_json::Value::Object(map) => map,
            _ => panic!("options must be a JSON object"),
        }
    }

    #[test]
    fn provider_names_are_postgres() {
        assert_eq!(PostgresCacheProvider.provider(), "postgres");
        assert_eq!(PostgresLockProvider.provider(), "postgres");
    }

    #[tokio::test]
    async fn build_cache_rejects_missing_connection_string_as_invalid_config() {
        // No `connection_string` field → deserialization fails before any pool
        // is opened, so this needs no container.
        let result = PostgresCacheProvider
            .build_cache(&options(json!({ "pool_max_size": 5 })))
            .await;
        assert!(
            matches!(result, Err(ClusterError::InvalidConfig { .. })),
            "a malformed options map must surface as InvalidConfig, got {:?}",
            result.as_ref().map(|_| ())
        );
    }

    #[tokio::test]
    async fn build_lock_rejects_missing_connection_string_as_invalid_config() {
        let result = PostgresLockProvider
            .build_lock(&options(json!({ "pool_max_size": 5 })))
            .await;
        assert!(
            matches!(result, Err(ClusterError::InvalidConfig { .. })),
            "a malformed options map must surface as InvalidConfig, got {:?}",
            result.as_ref().map(|_| ())
        );
    }

    #[tokio::test]
    async fn build_cache_rejects_unresolvable_env_var_as_invalid_config() {
        // A `${VAR}` with no value and no default fails at `expand_vars`, again
        // before any connection attempt.
        let result = PostgresCacheProvider
            .build_cache(&options(json!({
                "connection_string": "postgres://u:${PG_CLUSTER_PROVIDER_M5_UNSET}@h/db",
            })))
            .await;
        assert!(
            matches!(result, Err(ClusterError::InvalidConfig { .. })),
            "an unresolvable env var must surface as InvalidConfig, got {:?}",
            result.as_ref().map(|_| ())
        );
    }
}
