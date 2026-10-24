#![allow(clippy::expect_used, clippy::unwrap_used)]

use super::*;

fn parse(json: serde_json::Value) -> RatingConfig {
    serde_json::from_value(json).unwrap()
}

#[test]
fn the_default_configuration_is_valid_and_disables_every_event_source() {
    let config = RatingConfig::default();
    config.validate().unwrap();
    assert!(config.event_intake.sources.is_empty());
    assert_eq!(config.producer_policy().unwrap(), ProducerPolicy::default());
    assert_eq!(config.exception_sweep_interval(), Duration::from_mins(15));
    assert!(config.leadership.enabled);
}

#[test]
fn the_default_queues_are_child_work_rollup_and_one_delivery_epoch() {
    let layout = RatingConfig::default().queue_layout().unwrap();
    let names: Vec<String> = layout.queues().into_iter().map(|(name, _)| name).collect();
    assert_eq!(
        names,
        ["rating.child_work", "rating.rollup", "rating.delivery.e1"]
    );
}

#[test]
fn a_source_cannot_be_enabled_before_its_contract_is_adopted() {
    for source in InboxSource::ALL {
        let config = parse(serde_json::json!({
            "event_intake": {"sources": {source.as_str(): {"enabled": true, "producer": "owner"}}}
        }));
        assert_eq!(
            config.validate(),
            Err(ConfigError::ContractNotAdopted(source.as_str().to_owned()))
        );
    }
}

#[test]
fn a_disabled_source_may_be_listed() {
    let config = parse(serde_json::json!({
        "event_intake": {"sources": {"fact": {"enabled": false}}}
    }));
    config.validate().unwrap();
}

#[test]
fn an_unknown_source_is_refused() {
    let config = parse(serde_json::json!({
        "event_intake": {"sources": {"usage": {"enabled": false}}}
    }));
    assert_eq!(
        config.validate(),
        Err(ConfigError::UnknownSource("usage".to_owned()))
    );
}

#[test]
fn partitions_are_powers_of_two_up_to_sixty_four() {
    for bad in [0_u16, 3, 65, 128] {
        let config = parse(serde_json::json!({"queues": {"rollup_partitions": bad}}));
        assert!(matches!(
            config.validate(),
            Err(ConfigError::Partitions {
                field: "rollup_partitions",
                ..
            })
        ));
    }
    let config = parse(serde_json::json!({"queues": {"rollup_partitions": 64}}));
    config.validate().unwrap();
}

#[test]
fn feed_epochs_are_distinct_and_positive() {
    for bad in [
        serde_json::json!([]),
        serde_json::json!([0]),
        serde_json::json!([1, 1]),
    ] {
        let config = parse(serde_json::json!({"queues": {"feed_epochs": bad}}));
        assert_eq!(config.validate(), Err(ConfigError::FeedEpochs));
    }
    let config = parse(serde_json::json!({"queues": {"feed_epochs": [1, 2]}}));
    assert!(
        config
            .queue_layout()
            .unwrap()
            .queues()
            .iter()
            .any(|(name, _)| name == "rating.delivery.e2")
    );
}

#[test]
fn unknown_keys_are_refused() {
    assert!(serde_json::from_value::<RatingConfig>(serde_json::json!({"nope": 1})).is_err());
}

#[test]
fn an_election_timing_the_cluster_sdk_refuses_is_refused() {
    let config = parse(serde_json::json!({"leadership": {"max_missed_renewals": 0}}));
    assert!(
        matches!(config.validate(), Err(ConfigError::Leadership(ref reason)) if reason.contains("max_missed_renewals")),
        "{:?}",
        config.validate()
    );
    // With leadership disabled the timing drives nothing.
    let config = parse(serde_json::json!({
        "leadership": {"enabled": false, "max_missed_renewals": 0}
    }));
    assert_eq!(config.validate(), Ok(()));
}

#[test]
fn durations_must_be_positive() {
    for (section, field, name) in [
        ("leadership", "ttl_secs", "leadership.ttl_secs"),
        (
            "leadership",
            "stop_timeout_secs",
            "leadership.stop_timeout_secs",
        ),
        (
            "leadership",
            "resolve_retry_max_secs",
            "leadership.resolve_retry_max_secs",
        ),
        (
            "exception_sweep",
            "interval_secs",
            "exception_sweep.interval_secs",
        ),
    ] {
        let config = parse(serde_json::json!({ section: { field: 0 } }));
        assert_eq!(
            config.validate(),
            Err(ConfigError::ZeroDuration(name)),
            "{name}"
        );
    }
}

#[test]
fn the_leader_stop_budget_fits_inside_the_gears_stop_budget() {
    assert!(RatingConfig::default().leadership.stop_timeout_secs <= MAX_LEADER_STOP_SECS);
    let at_limit = parse(serde_json::json!({
        "leadership": {"stop_timeout_secs": MAX_LEADER_STOP_SECS}
    }));
    assert_eq!(at_limit.validate(), Ok(()));
    let beyond = parse(serde_json::json!({
        "leadership": {"stop_timeout_secs": MAX_LEADER_STOP_SECS + 1}
    }));
    assert_eq!(
        beyond.validate(),
        Err(ConfigError::LeaderStopTooLong(MAX_LEADER_STOP_SECS + 1))
    );
}

#[test]
fn a_producer_identity_is_refused_when_empty_padded_long_or_controlled() {
    for producer in [
        String::new(),
        " bss-subscriptions".to_owned(),
        "bss-subscriptions ".to_owned(),
        "bss\u{0}subscriptions".to_owned(),
        "p".repeat(MAX_PRODUCER_LEN + 1),
    ] {
        let config = parse(serde_json::json!({
            "event_intake": {"sources": {"fact": {"enabled": true, "producer": producer}}}
        }));
        assert_eq!(
            config.producer_policy(),
            Err(ConfigError::ProducerInvalid("fact".to_owned())),
            "{producer:?}"
        );
    }
    // A well-formed identity gets as far as the adoption gate.
    let config = parse(serde_json::json!({
        "event_intake": {"sources": {"fact": {"enabled": true, "producer": "bss-subscriptions"}}}
    }));
    assert_eq!(
        config.producer_policy(),
        Err(ConfigError::ContractNotAdopted("fact".to_owned()))
    );
}
