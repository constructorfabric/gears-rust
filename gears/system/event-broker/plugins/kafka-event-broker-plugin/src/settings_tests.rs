#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::collections::BTreeMap;
use std::time::Duration;

use event_broker_sdk::backend::BackendError;

use crate::settings::KafkaSettings;

fn parse(value: serde_json::Value) -> Result<KafkaSettings, BackendError> {
    let serde_json::Value::Object(settings) = value else {
        unreachable!("an object literal");
    };
    KafkaSettings::parse(&settings)
}

fn rejection(value: serde_json::Value) -> String {
    match parse(value) {
        Err(BackendError::InvalidConfig { detail }) => detail,
        other => panic!("expected InvalidConfig, got {other:?}"),
    }
}

#[test]
fn defaults_apply_beside_the_bootstrap_servers() {
    let parsed = parse(serde_json::json!({ "bootstrap_servers": "k1:9092,k2:9092" })).unwrap();
    assert_eq!(
        parsed,
        KafkaSettings {
            bootstrap_servers: "k1:9092,k2:9092".to_owned(),
            topic_prefix: "evbk".to_owned(),
            replication_factor: 3,
            min_insync_replicas: 2,
            fence_partitions: 16,
            poll_interval: Duration::from_millis(500),
            consumer_pool: 8,
            producer_pool: 16,
            request_timeout: Duration::from_secs(10),
            transaction_timeout: Duration::from_secs(10),
            client: BTreeMap::new(),
        }
    );
}

#[test]
fn every_setting_is_read() {
    let parsed = parse(serde_json::json!({
        "bootstrap_servers": "k:9092",
        "topic_prefix": "prod.eb-1_a",
        "replication_factor": 1,
        "min_insync_replicas": 1,
        "fence_partitions": 4,
        "poll_interval_ms": 250,
        "consumer_pool": 2,
        "producer_pool": 3,
        "request_timeout_ms": 7000,
        "transaction_timeout_ms": 30000,
        "client": { "client.id": "eb", "linger.ms": 5, "socket.keepalive.enable": true },
    }))
    .unwrap();
    assert_eq!(
        parsed,
        KafkaSettings {
            bootstrap_servers: "k:9092".to_owned(),
            topic_prefix: "prod.eb-1_a".to_owned(),
            replication_factor: 1,
            min_insync_replicas: 1,
            fence_partitions: 4,
            poll_interval: Duration::from_millis(250),
            consumer_pool: 2,
            producer_pool: 3,
            request_timeout: Duration::from_secs(7),
            transaction_timeout: Duration::from_secs(30),
            client: BTreeMap::from([
                ("client.id".to_owned(), "eb".to_owned()),
                ("linger.ms".to_owned(), "5".to_owned()),
                ("socket.keepalive.enable".to_owned(), "true".to_owned()),
            ]),
        }
    );
}

#[test]
fn bootstrap_servers_expand_environment_variables() {
    temp_env::with_var("EB_KAFKA_TEST_BOOTSTRAP", Some("k9:9092"), || {
        let parsed =
            parse(serde_json::json!({ "bootstrap_servers": "${EB_KAFKA_TEST_BOOTSTRAP}" }))
                .unwrap();
        assert_eq!(parsed.bootstrap_servers, "k9:9092");
    });
}

#[test]
fn rejections_name_the_setting_and_never_the_value() {
    let cases = [
        (serde_json::json!({}), "`bootstrap_servers` is required"),
        (
            serde_json::json!({ "bootstrap_servers": "" }),
            "`bootstrap_servers` must not be empty",
        ),
        (
            serde_json::json!({ "bootstrap_servers": 9092 }),
            "`bootstrap_servers` must be a string",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "${EB_KAFKA_TEST_UNSET_VAR}" }),
            "`bootstrap_servers` names an environment variable that is not set",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "topic_prefix": "bad/name" }),
            "`topic_prefix` must be 1 to 200 characters of [a-zA-Z0-9._-]",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "topic_prefix": ".." }),
            "`topic_prefix` must be 1 to 200 characters of [a-zA-Z0-9._-]",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "replication_factor": 0 }),
            "`replication_factor` must be an integer from 1 to 32",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "replication_factor": 2, "min_insync_replicas": 3 }),
            "`min_insync_replicas` must not exceed `replication_factor`",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "poll_interval_ms": 0 }),
            "`poll_interval_ms` must be a positive integer of milliseconds",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "consumer_pool": -1 }),
            "`consumer_pool` must be a positive integer",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "client": { "security.protocol": "SSL" } }),
            "`client.security.protocol` is not a librdkafka property this backend passes through",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "client": { "isolation.level": "read_uncommitted" } }),
            "`client.isolation.level` is not a librdkafka property this backend passes through",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "client": { "linger.ms": [1] } }),
            "`client.linger.ms` must be a string, a number or a boolean",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "client": "linger.ms=1" }),
            "`client` must be an object of librdkafka properties",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "producer_pool": 257 }),
            "`producer_pool` must be at most 256: each producer runs librdkafka threads and a connection per broker",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "client": { "socket.timeout.ms": 1000 } }),
            "`client.socket.timeout.ms` is not a librdkafka property this backend passes through",
        ),
        (
            serde_json::json!({ "bootstrap_servers": "k:1", "url": "x" }),
            "`url` is not a kafka backend setting",
        ),
    ];
    for (settings, expected) in cases {
        assert_eq!(rejection(settings.clone()), expected, "{settings}");
    }
}

#[test]
fn debug_hides_the_bootstrap_servers() {
    let parsed = parse(serde_json::json!({ "bootstrap_servers": "secret-host:9092" })).unwrap();
    let shown = format!("{parsed:?}");
    assert!(!shown.contains("secret-host"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
}

#[test]
fn the_request_timeout_fits_in_a_transaction() {
    assert_eq!(
        rejection(serde_json::json!({
            "bootstrap_servers": "k:1",
            "request_timeout_ms": 20_000,
            "transaction_timeout_ms": 10_000,
        })),
        "`request_timeout_ms` must not exceed `transaction_timeout_ms`: librdkafka refuses a socket timeout longer than the transaction timeout"
    );
}
