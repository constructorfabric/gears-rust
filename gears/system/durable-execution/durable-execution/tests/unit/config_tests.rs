use super::*;

#[test]
fn renewable_lease_does_not_bound_the_activity_contract_timeout() {
    let mut definition = crate::infra::storage::repository::tests::definition();
    definition.activities[0].timeout = Duration::from_mins(20);
    assert!(definition.validate().is_ok());
    assert!(Config::default().validate().is_ok());
}

#[test]
fn heartbeat_must_leave_time_to_renew() {
    let config = Config {
        heartbeat_secs: 40,
        ..Config::default()
    };
    assert!(config.validate().is_err());
}
#[test]
fn routes_are_versioned_exact_matches_and_unknown_revisions_use_default() {
    let config = Config {
        queues: [("durable-v1".into(), 2), ("fast".into(), 4)].into(),
        definition_queues: [("sync.v1".into(), "fast".into())].into(),
        ..Config::default()
    };
    assert!(config.validate().is_ok());
    assert_eq!(config.queue_for("sync.v1"), "fast");
    assert_eq!(config.queue_for("sync.v2"), "durable-v1");
    let mut bad = config;
    bad.definition_queues
        .insert("sync.v2".into(), "missing".into());
    assert!(bad.validate().is_err());
    bad.definition_queues.clear();
    bad.queues.insert("invalid.name".into(), 1);
    assert!(bad.validate().is_err());
}

#[test]
fn rejects_overflowing_timers_and_oversized_authorization_configuration() {
    let mut config = Config {
        dispatch_interval_secs: u64::MAX,
        ..Default::default()
    };
    assert!(config.validate().is_err());
    config.dispatch_interval_secs = 86_400;
    assert!(config.validate().is_ok());
    config.shutdown_timeout_secs = u64::MAX;
    assert!(config.validate().is_err());
    config = Config::default();
    config.queue_database_url_env = "A".repeat(129);
    assert!(config.validate().is_err());
    config.queue_database_url_env = "A".repeat(128);
    assert!(config.validate().is_ok());
    config.service_client_secret_env = "A".repeat(129);
    assert!(config.validate().is_err());
    config.service_client_secret_env = "SECRET".into();
    config.service_client_id = "a".repeat(256);
    assert!(config.validate().is_err());
    config.service_client_id = "a".repeat(255);
    config.service_scopes = vec!["a".repeat(255)];
    assert!(config.validate().is_ok());
    config.service_scopes[0].push('a');
    assert!(config.validate().is_err());
}
#[test]
fn routes_reject_malformed_or_oversized_definition_names() {
    for name in ["sync.v0".into(), "Sync.v1".into(), "x".repeat(161)] {
        let config = Config {
            definition_queues: [(name, "durable-v1".into())].into(),
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
}

#[test]
fn control_plane_failure_limit_has_finite_nonzero_bounds() {
    let mut config = Config::default();
    assert_eq!(config.control_plane_failure_limit, 5);
    for limit in [0, 129, u32::MAX] {
        config.control_plane_failure_limit = limit;
        assert!(config.validate().is_err());
    }
    for limit in [1, 128] {
        config.control_plane_failure_limit = limit;
        assert!(config.validate().is_ok());
    }
    let decoded: Config = serde_json::from_value(serde_json::json!({
        "control_plane_failure_limit": 9
    }))
    .unwrap();
    assert_eq!(decoded.control_plane_failure_limit, 9);
    assert!(decoded.validate().is_ok());
}
