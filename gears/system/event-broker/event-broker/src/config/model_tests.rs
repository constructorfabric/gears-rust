//! Per-topic settings: what an operator may write, and what the shapes admit.
//!
//! What a topic *resolves to* is not here. Resolution folds these entries
//! together with what the topic's own specification declares, which this type
//! cannot see, so the ladder and its outcomes live with
//! `crate::domain::resolution` and are tested there.

use std::time::Duration;

use serde_json::json;

use crate::config::{EventBrokerConfig, RetentionSize, StreamingConfig};

/// The backend types the fixtures name. Written out rather than imported from
/// the plugins: an operator writes these strings by hand too.
const MEMORY_BACKEND: &str = "gts.cf.core.events.backend.v1~cf.core.backend.memory.v1~";
const POSTGRES_BACKEND: &str = "gts.cf.core.events.backend.v1~cf.core.backend.postgres.v1~";

const TOPIC_TYPE: &str = "gts.cf.core.events.topic.v1~";
const USAGE_TOPIC: &str = "gts.cf.core.events.topic.v1~cf.billing.usage.topic.v1";
/// A whole `[modules.event_broker]` section, deserialized the way the platform
/// deserializes it, with `topics` supplied by the caller.
fn config_with_topics(topics: &serde_json::Value) -> EventBrokerConfig {
    serde_json::from_value(json!({
        "mode": "standalone",
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
        "topics": topics.clone(),
    }))
    .expect("test configuration deserializes")
}

#[test]
fn a_two_entry_map_deserializes_one_instance_less_key_and_one_fully_qualified() {
    let cfg = config_with_topics(&json!({
        TOPIC_TYPE: { "partitions": 8 },
        USAGE_TOPIC: { "partitions": 32 },
    }));

    assert_eq!(
        cfg.topics.entries.keys().collect::<Vec<_>>(),
        vec![TOPIC_TYPE, USAGE_TOPIC],
        "both keys are present, and neither was folded into the other"
    );
    assert_eq!(cfg.topics.entries[TOPIC_TYPE].partitions, Some(8));
    assert_eq!(cfg.topics.entries[USAGE_TOPIC].partitions, Some(32));
}

#[test]
fn retention_deserializes_a_humantime_duration_and_a_byte_count() {
    let cfg = config_with_topics(&json!({
        TOPIC_TYPE: {
            "partitions": 1,
            "retention": { "duration": "30d", "size_bytes": 128_000_000 },
        },
    }));

    let entry = cfg.topics.entries[TOPIC_TYPE]
        .retention
        .as_ref()
        .expect("the block was written");
    assert_eq!(entry.duration, Some(Duration::from_hours(24 * 30)));
    assert_eq!(entry.size_bytes, RetentionSize::Bytes(128_000_000));
}

/// The distinction the resolver depends on: an entry that omits `duration` is
/// not an entry that asked for the default. Only the absence is visible here;
/// what stands in its place when the topic itself declares a retention is the
/// resolver's to apply, and is asserted there.
#[test]
fn an_entry_that_states_no_duration_is_distinguishable_from_one_that_states_it() {
    let silent = config_with_topics(&json!({
        TOPIC_TYPE: { "partitions": 1, "retention": { "size_bytes": 4096 } },
    }));
    let stated = config_with_topics(&json!({
        TOPIC_TYPE: { "partitions": 1, "retention": { "duration": "168h" } },
    }));

    assert_eq!(
        silent.topics.entries[TOPIC_TYPE]
            .retention
            .as_ref()
            .and_then(|retention| retention.duration),
        None,
        "an omitted duration must stay absent rather than materialising as the default",
    );
    assert_eq!(
        stated.topics.entries[TOPIC_TYPE]
            .retention
            .as_ref()
            .and_then(|retention| retention.duration),
        Some(crate::config::DEFAULT_RETENTION_DURATION),
        "a duration written as the same value the default happens to be is still a statement",
    );
}

/// A byte bound has to be releasable, or a bounded type default would reach
/// every topic of that type with no way out.
#[test]
fn an_explicitly_unbounded_byte_bound_releases_a_topic_from_a_bounded_default() {
    let cfg = config_with_topics(&json!({
        TOPIC_TYPE: {
            "partitions": 1,
            "retention": { "duration": "30d", "size_bytes": 128_000_000 },
        },
        USAGE_TOPIC: { "retention": { "size_bytes": null } },
    }));

    assert_eq!(
        cfg.topics.entries[USAGE_TOPIC]
            .retention
            .as_ref()
            .map(|retention| retention.size_bytes),
        Some(RetentionSize::Unbounded),
        "an explicit null is a statement, not an omission",
    );
}

#[test]
fn an_unknown_key_in_a_topic_entry_is_a_deserialization_error() {
    let err = serde_json::from_value::<EventBrokerConfig>(json!({
        "mode": "standalone",
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
        "topics": { TOPIC_TYPE: { "partition": 8 } },
    }))
    .expect_err("a typo must not be silently ignored");
    assert!(
        err.to_string().contains("unknown field `partition`"),
        "expected the typo to be named, got: {err}"
    );
}

// ── The backend registry, as written ─────────────────────────────────────

/// A whole section with `backends` supplied by the caller and a route for
/// every entry, so a test sees exactly the entry it wrote.
fn config_with_backends(backends: &serde_json::Value) -> EventBrokerConfig {
    serde_json::from_value(json!({
        "mode": "standalone",
        "backends": backends.clone(),
        "backend_routing": { TOPIC_TYPE: "main" },
    }))
    .expect("test configuration deserializes")
}

#[test]
fn a_backends_entry_holds_its_type_and_its_settings_as_written() {
    let cfg = config_with_backends(&json!({
        "main": { "type": POSTGRES_BACKEND, "url": "${EB_MAIN_URL}", "pool_size": 8 },
    }));

    let entry = cfg.backends.get("main").expect("the entry is kept by name");
    assert_eq!(entry.r#type.as_ref(), POSTGRES_BACKEND);
    assert_eq!(
        serde_json::Value::Object(entry.settings.clone()),
        json!({ "url": "${EB_MAIN_URL}", "pool_size": 8 }),
        "settings are held verbatim, `${{VAR}}` unexpanded: the provider expands it"
    );
    assert_eq!(
        cfg.backend_routing,
        std::collections::BTreeMap::from([(TOPIC_TYPE.to_owned(), "main".to_owned())])
    );
}

/// The gear holds a backend's own settings opaque, so an unrecognised key
/// travels to the plugin instead of being refused here - the plugin's own
/// settings type is what can judge it.
#[test]
fn an_unrecognised_key_inside_a_backends_entry_reaches_the_backend() {
    let cfg =
        config_with_backends(&json!({ "main": { "type": MEMORY_BACKEND, "pathh": ":memory:" } }));

    let entry = cfg.backends.get("main").expect("the entry is kept by name");
    assert_eq!(
        serde_json::Value::Object(entry.settings.clone()),
        json!({ "pathh": ":memory:" }),
        "the typo is handed on verbatim; the plugin's own type is what rejects it"
    );
}

/// A settings value may be a credential, and a `{:?}` of the configuration
/// lands in panics, assertions and logs (feature 0005 AC-5).
#[test]
fn debug_of_a_backends_entry_prints_no_settings_value() {
    let cfg = config_with_backends(&json!({
        "main": { "type": POSTGRES_BACKEND, "url": "postgres://eb:hunter2@db:5432/eb" },
    }));

    assert_eq!(
        format!(
            "{:?}",
            cfg.backends.get("main").expect("the entry is kept by name")
        ),
        format!(
            "BackendEntry {{ type: GtsTypeId(GtsEntityId(\"{POSTGRES_BACKEND}\")), settings: <1 keys> }}"
        )
    );
    assert!(!format!("{cfg:?}").contains("hunter2"));
}

/// An entry that names no type is a mistake, not a request for some default:
/// there is none.
#[test]
fn a_backends_entry_with_no_type_fails_to_deserialize() {
    let err = serde_json::from_value::<EventBrokerConfig>(json!({
        "mode": "standalone",
        "backends": { "main": { "path": ":memory:" } },
        "backend_routing": { TOPIC_TYPE: "main" },
    }))
    .expect_err("an entry must say which backend");
    assert_eq!(err.to_string(), "missing field `type`");
}

/// A short alias is not a backend type. Failing at load beats resolving to
/// whichever backend happened to be linked into the build.
#[test]
fn a_short_alias_as_a_backend_type_fails_to_deserialize() {
    let err = serde_json::from_value::<EventBrokerConfig>(json!({
        "mode": "standalone",
        "backends": { "main": { "type": "sqlite" } },
        "backend_routing": { TOPIC_TYPE: "main" },
    }))
    .expect_err("a backend is named by its GTS type");
    assert!(
        err.to_string().contains("sqlite"),
        "expected the rejected value to be named, got: {err}"
    );
}

/// `default_storage_backend` and the per-topic `backend` block do not exist:
/// `backends` and `backend_routing` replace them, and a configuration still
/// carrying either is refused rather than half-read (feature 0005 §2).
#[test]
fn the_removed_default_storage_backend_key_is_refused() {
    let err = serde_json::from_value::<EventBrokerConfig>(json!({
        "mode": "standalone",
        "default_storage_backend": MEMORY_BACKEND,
    }))
    .expect_err("the key no longer exists");
    assert_eq!(
        err.to_string(),
        "unknown field `default_storage_backend`, expected one of `mode`, `batch`, `polling`, `subscription`, `streaming`, `loader`, `workers`, `registration`, `provisioning`, `topics`, `backends`, `backend_routing`, `outbox_groups`"
    );
}

/// A chain row lives as long as its producer registration and has no retention
/// of its own (feature 0001 §4.7), so `producer.state_retention` does not
/// exist, and neither does the `producer` section that held it.
#[test]
fn the_removed_producer_key_is_refused() {
    let err = serde_json::from_value::<EventBrokerConfig>(json!({
        "mode": "standalone",
        "producer": { "state_retention": "P14D" },
    }))
    .expect_err("the key no longer exists");
    assert_eq!(
        err.to_string(),
        "unknown field `producer`, expected one of `mode`, `batch`, `polling`, `subscription`, `streaming`, `loader`, `workers`, `registration`, `provisioning`, `topics`, `backends`, `backend_routing`, `outbox_groups`"
    );
}

#[test]
fn an_outbox_group_entry_holds_its_partitions_and_topics_as_written() {
    let cfg: EventBrokerConfig = serde_json::from_value(json!({
        "mode": "standalone",
        "outbox_groups": {
            "default": { "partitions": 300 },
            "audit": { "partitions": 4, "topics": [USAGE_TOPIC] },
        },
    }))
    .expect("held as written; the startup checks judge it");

    let default = &cfg.outbox_groups["default"];
    assert_eq!((default.partitions, default.topics.clone()), (300, None));
    let audit = &cfg.outbox_groups["audit"];
    assert_eq!(
        (audit.partitions, audit.topics.clone()),
        (4, Some(vec![USAGE_TOPIC.to_owned()]))
    );
}

#[test]
fn an_outbox_group_entry_refuses_an_unknown_key() {
    let err = serde_json::from_value::<EventBrokerConfig>(json!({
        "mode": "standalone",
        "outbox_groups": { "default": { "partitions": 16, "partition": 4 } },
    }))
    .expect_err("a typo is not silently ignored");
    assert_eq!(
        err.to_string(),
        "unknown field `partition`, expected `partitions` or `topics`"
    );
}

#[test]
fn a_per_topic_backend_block_is_refused() {
    let err = serde_json::from_value::<EventBrokerConfig>(json!({
        "mode": "standalone",
        "topics": { TOPIC_TYPE: { "backend": { "type": MEMORY_BACKEND } } },
    }))
    .expect_err("the block no longer exists");
    assert_eq!(
        err.to_string(),
        "unknown field `backend`, expected `partitions` or `retention`"
    );
}

/// A valid `[streaming]` section with every field set, so a rejection test can
/// override exactly one field and know the rejection is that field's.
fn streaming_json() -> serde_json::Value {
    json!({
        "heartbeat_interval_secs": 5,
        "read_batch_max_events": 256,
        "read_batch_max_bytes": 1_048_576,
        "progress_drift_threshold": 1000,
        "progress_min_interval_secs": 30,
    })
}

#[test]
fn a_zero_heartbeat_interval_is_a_deserialization_error() {
    let mut streaming = streaming_json();
    streaming["heartbeat_interval_secs"] = json!(0);
    let err = serde_json::from_value::<StreamingConfig>(streaming)
        .expect_err("zero would spin the delivery session loop");
    assert!(
        err.to_string().contains("at least 1"),
        "expected the reason to name the rule, got: {err}"
    );
}

#[test]
fn a_zero_progress_min_interval_is_a_deserialization_error() {
    let mut streaming = streaming_json();
    streaming["progress_min_interval_secs"] = json!(0);
    let err = serde_json::from_value::<StreamingConfig>(streaming)
        .expect_err("zero would spin the delivery session loop");
    assert!(
        err.to_string().contains("at least 1"),
        "expected the reason to name the rule, got: {err}"
    );
}

/// An operator overriding one streaming knob writes only that knob; every
/// other field keeps its default.
#[test]
fn a_partial_streaming_block_keeps_the_defaults_it_does_not_state() {
    let cfg: EventBrokerConfig = serde_json::from_value(json!({
        "mode": "standalone",
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
        "streaming": { "read_batch_max_events": 7 },
    }))
    .expect("a partial streaming block deserializes");

    let expected = StreamingConfig {
        read_batch_max_events: 7,
        ..StreamingConfig::default()
    };
    assert_eq!(format!("{:?}", cfg.streaming), format!("{expected:?}"));
}

/// Every tunable section accepts an empty block, which is the same as leaving
/// the section out.
#[test]
fn an_empty_block_in_every_section_deserializes_to_its_defaults() {
    let stated: EventBrokerConfig = serde_json::from_value(json!({
        "mode": "standalone",
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
        "batch": {},
        "polling": {},
        "subscription": {},
        "streaming": {},
        "loader": {},
        "workers": {},
        "registration": {},
        "provisioning": {},
    }))
    .expect("empty sections deserialize");
    let omitted: EventBrokerConfig = serde_json::from_value(json!({
        "mode": "standalone",
        "backends": { "main": { "type": MEMORY_BACKEND } },
        "backend_routing": { "gts.cf.core.events.topic.v1~": "main" },
    }))
    .expect("omitted sections deserialize");

    assert_eq!(format!("{stated:?}"), format!("{omitted:?}"));
}
