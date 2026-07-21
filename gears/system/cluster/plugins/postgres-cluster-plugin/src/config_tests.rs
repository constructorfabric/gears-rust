use super::*;
use serde_json::json;
use toolkit::var_expand::ExpandVars;

#[test]
fn cluster_config_applies_documented_defaults() {
    let config: PostgresClusterConfig =
        serde_json::from_value(json!({ "connection_string": "postgres://u@h/db" }))
            .expect("minimal config deserializes");
    assert_eq!(config.pool_max_size, 5);
    assert_eq!(config.pool_acquire_timeout_ms, 5_000);
    assert_eq!(config.schema, "public");
    assert_eq!(config.cache_reaper_interval_ms, 10_000);
    assert_eq!(config.lock_reaper_interval_ms, 5_000);
    assert_eq!(config.sd_poll_interval_ms, 5_000);
    assert!(!config.pgbouncer_transaction_mode);
    assert_eq!(config.lock_name_cardinality_warn_threshold, 1_000);
    assert_eq!(config.replication_mode, None);
}

#[test]
fn lock_config_applies_documented_defaults() {
    let config: PostgresLockConfig =
        serde_json::from_value(json!({ "connection_string": "postgres://u@h/db" }))
            .expect("minimal lock config deserializes");
    assert_eq!(config.pool_max_size, 5);
    assert_eq!(config.pool_acquire_timeout_ms, 5_000);
    assert_eq!(config.schema, "public");
    assert_eq!(config.lock_reaper_interval_ms, 5_000);
    assert!(!config.pgbouncer_transaction_mode);
    assert_eq!(config.lock_name_cardinality_warn_threshold, 1_000);
    assert_eq!(config.replication_mode, None);
}

#[test]
fn cluster_config_round_trips_every_field() {
    let config: PostgresClusterConfig = serde_json::from_value(json!({
        "connection_string": "postgres://u@h/db",
        "pool_max_size": 12,
        "pool_acquire_timeout_ms": 1_234,
        "schema": "cluster",
        "cache_reaper_interval_ms": 2_222,
        "lock_reaper_interval_ms": 3_333,
        "sd_poll_interval_ms": 4_444,
        "pgbouncer_transaction_mode": true,
        "lock_name_cardinality_warn_threshold": 42,
        "replication_mode": "sync",
    }))
    .expect("full config deserializes");
    assert_eq!(config.pool_max_size, 12);
    assert_eq!(config.pool_acquire_timeout_ms, 1_234);
    assert_eq!(config.schema, "cluster");
    assert_eq!(config.cache_reaper_interval_ms, 2_222);
    assert_eq!(config.lock_reaper_interval_ms, 3_333);
    assert_eq!(config.sd_poll_interval_ms, 4_444);
    assert!(config.pgbouncer_transaction_mode);
    assert_eq!(config.lock_name_cardinality_warn_threshold, 42);
    assert_eq!(config.replication_mode, Some(ReplicationMode::Sync));
}

#[test]
fn replication_mode_variants_round_trip() {
    for (raw, expected) in [
        ("async", ReplicationMode::Async),
        ("sync", ReplicationMode::Sync),
    ] {
        let config: PostgresClusterConfig = serde_json::from_value(json!({
            "connection_string": "postgres://u@h/db",
            "replication_mode": raw,
        }))
        .expect("config with replication_mode deserializes");
        assert_eq!(config.replication_mode, Some(expected));
    }
}

#[test]
fn unknown_replication_mode_variant_is_rejected() {
    let result: Result<PostgresClusterConfig, _> = serde_json::from_value(json!({
        "connection_string": "postgres://u@h/db",
        "replication_mode": "semi_sync",
    }));
    assert!(
        result.is_err(),
        "an unknown replication_mode variant must be rejected"
    );
}

#[test]
fn unknown_field_is_rejected() {
    // `deny_unknown_fields` on both config types.
    let cluster: Result<PostgresClusterConfig, _> = serde_json::from_value(json!({
        "connection_string": "postgres://u@h/db",
        "not_a_real_field": true,
    }));
    assert!(
        cluster.is_err(),
        "cluster config must reject unknown fields"
    );

    // A cache-only field on the lock config is likewise unknown to it.
    let lock: Result<PostgresLockConfig, _> = serde_json::from_value(json!({
        "connection_string": "postgres://u@h/db",
        "sd_poll_interval_ms": 1_000,
    }));
    assert!(lock.is_err(), "lock config must reject cache-only fields");
}

#[test]
fn connection_string_expands_default_when_var_unset() {
    let mut config: PostgresClusterConfig = serde_json::from_value(json!({
        "connection_string": "postgres://u:${PG_CLUSTER_PW_M5_UNSET:-fallbackpw}@h/db",
    }))
    .expect("config deserializes");
    config
        .expand_vars()
        .expect("expansion with a default succeeds");
    assert_eq!(config.connection_string, "postgres://u:fallbackpw@h/db");
}

#[test]
fn missing_env_var_without_default_surfaces_as_error() {
    let mut config: PostgresClusterConfig = serde_json::from_value(json!({
        "connection_string": "postgres://u:${PG_CLUSTER_PW_M5_UNSET_NODEFAULT}@h/db",
    }))
    .expect("config deserializes");
    assert!(
        config.expand_vars().is_err(),
        "a referenced env var with no default and no value must surface as an error"
    );
}

#[test]
fn debug_masks_the_connection_string() {
    // PGR-M9: the DSN (which embeds the DB password after expansion) must
    // never appear in `{:?}` output.
    let cluster: PostgresClusterConfig = serde_json::from_value(json!({
        "connection_string": "postgres://user:supersecret@h/db",
    }))
    .expect("config deserializes");
    let rendered = format!("{cluster:?}");
    assert!(
        !rendered.contains("supersecret"),
        "Debug must not leak the password"
    );
    assert!(
        !rendered.contains("postgres://"),
        "Debug must not leak the DSN"
    );
    assert!(
        rendered.contains(REDACTED_DSN),
        "Debug must show the redaction marker"
    );

    let lock: PostgresLockConfig = serde_json::from_value(json!({
        "connection_string": "postgres://user:supersecret@h/db",
    }))
    .expect("lock config deserializes");
    let rendered = format!("{lock:?}");
    assert!(
        !rendered.contains("supersecret"),
        "lock Debug must not leak the password"
    );
    assert!(
        rendered.contains(REDACTED_DSN),
        "lock Debug must show the redaction marker"
    );
}
