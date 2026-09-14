//! Every section of the operator config must be omissible, since an operator
//! who states nothing should still get a working broker.

use super::config::{EventBrokerConfig, ProducerConfig, TopicConfig};

/// The minimal config an operator can write: a mode and a backend. Everything
/// else has to come from a default, or the broker cannot start without a full
/// file in front of it.
#[test]
fn every_optional_section_defaults_when_absent() {
    let config: EventBrokerConfig = serde_json::from_value(serde_json::json!({
        "mode": "standalone",
        "default_storage_backend": "database"
    }))
    .expect("a config naming only the required fields parses");

    let defaults = TopicConfig::default();
    assert_eq!(config.topic.partitions, defaults.partitions);
    assert_eq!(config.topic.retention, defaults.retention);
    assert_eq!(
        config.producer.state_retention,
        ProducerConfig::default().state_retention
    );
}

/// The producer's deduplication window is a separate dial from the topic's event
/// retention, and its default is the `P14D` cap itself.
#[test]
fn producer_state_retention_defaults_to_the_fourteen_day_cap() {
    assert_eq!(
        ProducerConfig::default().state_retention.to_string(),
        "PT336H"
    );
}

/// The two topic defaults are the broker's answer for a topic that states
/// neither, so they are asserted by value rather than by round-trip.
#[test]
fn topic_defaults_are_eight_partitions_and_thirty_days() {
    let defaults = TopicConfig::default();
    assert_eq!(defaults.partitions, 8);
    assert_eq!(defaults.retention.to_string(), "PT720H");
}

/// An operator overriding one knob must not lose the other.
#[test]
fn overriding_one_topic_default_keeps_the_other() {
    let config: EventBrokerConfig = serde_json::from_value(serde_json::json!({
        "mode": "standalone",
        "default_storage_backend": "database",
        "topic": { "partitions": 16, "retention": "P7D" }
    }))
    .expect("an explicit topic section parses");

    assert_eq!(config.topic.partitions, 16);
    assert_eq!(config.topic.retention.to_string(), "PT168H");
}
