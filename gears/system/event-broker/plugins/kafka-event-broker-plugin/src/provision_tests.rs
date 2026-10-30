use std::collections::BTreeMap;
use std::time::Duration;

use event_broker_sdk::GtsInstanceId;
use event_broker_sdk::backend::{Retention, TopicSpec};

use crate::provision::topic_configs;

fn spec(retention: Retention) -> TopicSpec {
    TopicSpec {
        topic: GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.topic.v1")
            .expect("valid id"),
        partitions: 2,
        retention,
    }
}

fn configs(pairs: &[(&'static str, &str)]) -> BTreeMap<&'static str, String> {
    pairs.iter().map(|(k, v)| (*k, (*v).to_owned())).collect()
}

#[test]
fn unset_bounds_do_not_limit() {
    assert_eq!(
        topic_configs(&spec(Retention::default()), 2),
        configs(&[
            ("cleanup.policy", "delete"),
            ("message.timestamp.type", "LogAppendTime"),
            ("min.insync.replicas", "2"),
            ("retention.bytes", "-1"),
            ("retention.ms", "-1"),
        ])
    );
}

#[test]
fn an_age_bound_also_bounds_the_segment() {
    assert_eq!(
        topic_configs(
            &spec(Retention {
                max_age: Some(Duration::from_millis(1500)),
                max_bytes: Some(4096),
            }),
            1
        ),
        configs(&[
            ("cleanup.policy", "delete"),
            ("message.timestamp.type", "LogAppendTime"),
            ("min.insync.replicas", "1"),
            ("retention.bytes", "4096"),
            ("retention.ms", "1500"),
            ("segment.ms", "1500"),
        ])
    );
    assert_eq!(
        topic_configs(
            &spec(Retention {
                max_age: Some(Duration::from_hours(24 * 30)),
                max_bytes: None,
            }),
            1
        )
        .get("segment.ms")
        .map(String::as_str),
        Some("604800000")
    );
}
