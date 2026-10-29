#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::time::Duration;

use super::{
    ConfigError, MAX_CALL_TIMEOUT_SECS, MAX_FEED_BACKOFF_SECS, MAX_LEADER_STOP_SECS,
    MAX_LEADER_TTL_SECS, MAX_POLL_INTERVAL_SECS, MAX_PRODUCER_LEN, MAX_RESOLVE_RETRY_SECS,
    MAX_RETENTION_FLOOR_DAYS, MAX_SUBSCRIPTIONS, MAX_SWEEP_INTERVAL_SECS, RatingConfig,
    ResolvedConfig,
};
use crate::domain::error::Cause;
use crate::domain::inbox::{InboxSource, ProducerPolicy};
use crate::domain::usage::feed::{MAX_PAGE_LIMIT, MeterId};
use crate::infra::queues::MAX_FEED_EPOCHS;

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
        serde_json::json!((1..=u32::try_from(MAX_FEED_EPOCHS).unwrap() + 1).collect::<Vec<_>>()),
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
    let refused = config.validate().unwrap_err();
    assert!(
        matches!(refused, ConfigError::Leadership(ref cause) if cause.to_string().contains("max_missed_renewals")),
        "{refused:?}"
    );
    // The Cluster SDK's error stays in the boot error chain.
    assert!(
        std::error::Error::source(&refused)
            .and_then(<dyn std::error::Error>::downcast_ref::<Cause>)
            .is_some_and(|cause| cause.get().is::<cluster_sdk::ClusterError>()),
        "{refused:?}"
    );
    // With leadership disabled the timing drives nothing.
    let config = parse(serde_json::json!({
        "leadership": {"enabled": false, "max_missed_renewals": 0}
    }));
    assert_eq!(config.validate(), Ok(()));
}

#[test]
fn every_duration_is_at_least_one_second_and_at_most_its_bound() {
    for (section, field, name, max) in [
        (
            "leadership",
            "ttl_secs",
            "leadership.ttl_secs",
            MAX_LEADER_TTL_SECS,
        ),
        (
            "leadership",
            "stop_timeout_secs",
            "leadership.stop_timeout_secs",
            MAX_LEADER_STOP_SECS,
        ),
        (
            "leadership",
            "resolve_retry_max_secs",
            "leadership.resolve_retry_max_secs",
            MAX_RESOLVE_RETRY_SECS,
        ),
        (
            "exception_sweep",
            "interval_secs",
            "exception_sweep.interval_secs",
            MAX_SWEEP_INTERVAL_SECS,
        ),
    ] {
        for value in [0, max + 1, 90_000_000] {
            let config = parse(serde_json::json!({ section: { field: value } }));
            assert_eq!(
                config.validate(),
                Err(ConfigError::DurationOutOfRange {
                    field: name,
                    value,
                    max
                }),
                "{name} = {value}"
            );
        }
    }
    // Each bound itself is accepted (the TTL with the renewals it leaves room for).
    let at_bounds = parse(serde_json::json!({
        "leadership": {
            "ttl_secs": MAX_LEADER_TTL_SECS,
            "stop_timeout_secs": MAX_LEADER_STOP_SECS,
            "resolve_retry_max_secs": MAX_RESOLVE_RETRY_SECS
        },
        "exception_sweep": {"interval_secs": MAX_SWEEP_INTERVAL_SECS}
    }));
    assert_eq!(at_bounds.validate(), Ok(()));
}

#[test]
fn the_leader_stop_budget_fits_inside_the_gears_stop_budget() {
    assert!(RatingConfig::default().leadership.stop_timeout_secs <= MAX_LEADER_STOP_SECS);
}

#[test]
fn the_resolved_configuration_is_what_was_checked() {
    let resolved = parse(serde_json::json!({"queues": {"feed_epochs": [1, 2]}}))
        .resolve()
        .unwrap();
    assert_eq!(resolved.queues.feed_epochs(), &[1, 2]);
    assert_eq!(resolved.producers, ProducerPolicy::default());
    let election = resolved.election.expect("leadership is on by default");
    assert_eq!(election.ttl(), Duration::from_secs(30));
    let disabled = parse(serde_json::json!({"leadership": {"enabled": false}}))
        .resolve()
        .unwrap();
    assert!(disabled.election.is_none());
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

const HOURS: &str = "gts.cf.core.uc.usage_record.v2~cf.test.hours.v1~";
const GB: &str = "gts.cf.core.uc.usage_record.v2~cf.test.gb.v1~";
const ROOT: &str = "00000000-0000-0000-0000-000000000010";
const OTHER_ROOT: &str = "00000000-0000-0000-0000-000000000020";

#[allow(
    clippy::needless_pass_by_value,
    reason = "each call builds its value in place"
)]
fn usage_feed(feed: serde_json::Value) -> Result<ResolvedConfig, ConfigError> {
    parse(serde_json::json!({ "usage_feed": feed })).resolve()
}

#[test]
fn the_usage_feed_reads_nothing_by_default() {
    let resolved = usage_feed(serde_json::json!({})).unwrap();
    assert!(resolved.usage.sources.is_empty());
    assert_eq!(resolved.usage.config_generation, 0);
    let reader = resolved.usage.reader;
    assert_eq!(reader.page_limit(), MAX_PAGE_LIMIT);
    assert_eq!(reader.poll_interval(), Duration::from_secs(30));
    assert_eq!(reader.backoff_cap(), Duration::from_secs(300));
    assert_eq!(reader.call_timeout(), Duration::from_secs(30));
    assert_eq!(reader.max_queue_depth(), 1_000_000);
    assert_eq!(
        resolved.usage.retention_floor,
        Duration::from_hours(125 * 24)
    );
}

#[test]
fn a_subscription_resolves_to_its_source_and_the_retention_floor_to_seconds() {
    let resolved = usage_feed(serde_json::json!({
        "subscriptions": [{"meters": [GB, HOURS], "tenant_scope": [OTHER_ROOT, ROOT]}],
        "config_generation": 7,
        "retention_floor_days": 2,
    }))
    .unwrap();
    assert_eq!(resolved.usage.config_generation, 7);
    assert_eq!(resolved.usage.retention_floor, Duration::from_hours(48));
    let [source] = resolved.usage.sources.as_slice() else {
        panic!("one source, got {:?}", resolved.usage.sources);
    };
    let meters: Vec<&str> = source
        .subscription()
        .meters()
        .iter()
        .map(MeterId::as_str)
        .collect();
    assert_eq!(meters, [GB, HOURS], "sorted");
    assert_eq!(source.tenant_scope().owner().to_string(), ROOT);
}

#[test]
fn the_page_limit_is_one_to_a_thousand() {
    for limit in [0, MAX_PAGE_LIMIT + 1] {
        assert_eq!(
            usage_feed(serde_json::json!({"page_limit": limit})).map(drop),
            Err(ConfigError::PageLimit(limit))
        );
    }
    for limit in [1, MAX_PAGE_LIMIT] {
        assert!(usage_feed(serde_json::json!({"page_limit": limit})).is_ok());
    }
}

#[test]
fn the_queue_depth_limit_is_at_least_one() {
    assert_eq!(
        usage_feed(serde_json::json!({"max_queue_depth": 0})).map(drop),
        Err(ConfigError::QueueDepth)
    );
}

#[test]
fn every_usage_feed_duration_is_at_least_one_second_and_at_most_its_bound() {
    for (field, max) in [
        ("poll_interval_secs", MAX_POLL_INTERVAL_SECS),
        ("backoff_max_secs", MAX_FEED_BACKOFF_SECS),
        ("retention_floor_days", u64::from(MAX_RETENTION_FLOOR_DAYS)),
        ("call_timeout_secs", MAX_CALL_TIMEOUT_SECS),
    ] {
        for value in [0, max + 1] {
            let refused = usage_feed(serde_json::json!({ field: value })).map(drop);
            assert!(
                matches!(refused, Err(ConfigError::DurationOutOfRange { value: v, max: m, .. }) if v == value && m == max),
                "{field} = {value}: {refused:?}"
            );
        }
        assert!(
            usage_feed(serde_json::json!({ field: max })).is_ok(),
            "{field} = {max}"
        );
    }
}

#[test]
fn a_subscription_listed_twice_in_another_order_is_refused() {
    let refused = usage_feed(serde_json::json!({
        "subscriptions": [
            {"meters": [HOURS, GB], "tenant_scope": [ROOT, OTHER_ROOT]},
            {"meters": [GB, HOURS], "tenant_scope": [OTHER_ROOT, ROOT]},
        ],
    }));
    assert_eq!(
        refused.map(drop),
        Err(ConfigError::DuplicateSubscription { index: 1 })
    );
}

#[test]
fn a_malformed_subscription_is_refused_with_its_index() {
    for (subscription, what) in [
        (
            serde_json::json!({"meters": ["gts.other~"], "tenant_scope": [ROOT]}),
            "a malformed meter",
        ),
        (
            serde_json::json!({"meters": [], "tenant_scope": [ROOT]}),
            "no meter",
        ),
        (
            serde_json::json!({"meters": [HOURS], "tenant_scope": []}),
            "no root",
        ),
    ] {
        let refused = usage_feed(serde_json::json!({
            "subscriptions": [{"meters": [GB], "tenant_scope": [ROOT]}, subscription],
        }));
        assert!(
            matches!(refused, Err(ConfigError::FeedSubscription { index: 1, .. })),
            "{what}: {refused:?}"
        );
    }
}

#[test]
fn at_most_thirty_two_subscriptions_are_read() {
    let subscriptions = |count: usize| -> Vec<serde_json::Value> {
        (0..count)
            .map(|n| {
                serde_json::json!({
                    "meters": [format!("gts.cf.core.uc.usage_record.v2~cf.test.m{n}.v1~")],
                    "tenant_scope": [ROOT],
                })
            })
            .collect()
    };
    assert!(
        usage_feed(serde_json::json!({"subscriptions": subscriptions(MAX_SUBSCRIPTIONS)})).is_ok()
    );
    assert_eq!(
        usage_feed(serde_json::json!({"subscriptions": subscriptions(MAX_SUBSCRIPTIONS + 1)}))
            .map(drop),
        Err(ConfigError::TooManySubscriptions(MAX_SUBSCRIPTIONS + 1))
    );
}
