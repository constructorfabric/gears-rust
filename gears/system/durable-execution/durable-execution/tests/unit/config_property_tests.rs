use super::*;
use proptest::prelude::*;

#[test]
fn validation_boundaries_are_enforced_before_runtime_start() {
    for concurrency in [0, 1, 128, 129] {
        let config = Config {
            queues: [("durable-v1".into(), concurrency)].into(),
            ..Default::default()
        };
        assert_eq!(config.validate().is_ok(), (1..=128).contains(&concurrency));
    }
    for seconds in [0, 1, 86_400, 86_401] {
        let config = Config {
            dispatch_interval_secs: seconds,
            ..Default::default()
        };
        assert_eq!(config.validate().is_ok(), (1..=86_400).contains(&seconds));
    }
    for (heartbeat, lease, valid) in [
        (0, 120, false),
        (39, 120, true),
        (40, 120, false),
        (41, 120, false),
        (1, 0, false),
        (1, 4, true),
        (1, 86_401, false),
        (u64::MAX, u64::MAX, false),
    ] {
        let config = Config {
            lease_secs: lease,
            heartbeat_secs: heartbeat,
            ..Default::default()
        };
        assert_eq!(
            config.validate().is_ok(),
            valid,
            "heartbeat={heartbeat}, lease={lease}"
        );
    }
    for value in [
        "",
        "lower_case",
        "DATABASE-URL",
        "DATABASE.URL",
        "DATABASE URL",
    ] {
        let config = Config {
            queue_database_url_env: value.into(),
            ..Default::default()
        };
        assert!(config.validate().is_err(), "{value:?}");
    }
    for execute in [false, true] {
        let mut config = Config {
            execute_activities: execute,
            delivery_enabled: !execute,
            ..Default::default()
        };
        assert!(config.validate().is_err());
        config.service_client_id = "worker".into();
        assert!(config.validate().is_ok());
        config.service_client_secret_env = "secret-value".into();
        assert!(config.validate().is_err());
    }
    for (dispatch, shutdown) in [(0, 1), (1, 0)] {
        let config = Config {
            dispatch_interval_secs: dispatch,
            shutdown_timeout_secs: shutdown,
            ..Default::default()
        };
        assert!(config.validate().is_err());
    }
    let config = Config {
        default_queue: "absent".into(),
        ..Default::default()
    };
    assert!(config.validate().is_err());
    let config = Config {
        definition_queues: [(String::new(), "durable-v1".into())].into(),
        ..Default::default()
    };
    assert!(config.validate().is_err());
}
proptest! {
    #![proptest_config(ProptestConfig { cases:128, failure_persistence:Some(Box::new(proptest::test_runner::FileFailurePersistence::Direct(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/unit/config_property.proptest-regressions")))), ..ProptestConfig::default() })]
    #[test]
    fn queue_name_boundaries_follow_identifier_contract(name in "[a-zA-Z0-9_-]{0,105}") {
        let config = Config { default_queue: name.clone(), queues: [(name.clone(), 1)].into(), ..Default::default() };
        prop_assert_eq!(config.validate().is_ok(), (1..=100).contains(&name.len()));
    }
    #[test]
    fn invalid_queue_characters_are_never_accepted(prefix in "[a-z]{0,20}", suffix in "[a-z]{0,20}", bad in prop::sample::select(vec!['.', ' ', '/', '\n', '\u{0410}'])) {
        let name = format!("{prefix}{bad}{suffix}");
        let config = Config { default_queue: name.clone(), queues: [(name, 1)].into(), ..Default::default() };
        prop_assert!(config.validate().is_err());
    }
    #[test]
    fn arbitrary_heartbeat_settings_obey_strict_renewal_budget(heartbeat in any::<u64>(), lease in any::<u64>()) {
        let config = Config { lease_secs: lease, heartbeat_secs: heartbeat, ..Default::default() };
        let expected = heartbeat > 0 && lease <= 86_400 && u128::from(heartbeat) * 3 < u128::from(lease);
        prop_assert_eq!(config.validate().is_ok(), expected);
    }
    #[test]
    fn misspelled_config_fields_are_rejected(field in "unknown_[a-z]{1,20}", value in any::<u64>()) {
        let json = serde_json::json!({field: value});
        prop_assert!(serde_json::from_value::<Config>(json).is_err());
    }
}
