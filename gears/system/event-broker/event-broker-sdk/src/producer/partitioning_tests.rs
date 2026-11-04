//! Producer outbox placement: by topic and partition-key value.

use super::partitioning::producer_outbox_partition;
use toolkit_gts::GTS_ID_PREFIX;

#[test]
fn placement_vector_is_pinned() {
    let topic = format!("{GTS_ID_PREFIX}cf.core.events.topic.v1~example.sdk.outbox.orders.v1");
    assert_eq!(producer_outbox_partition(&topic, "15", 4), 0);
}

#[test]
fn one_key_of_one_topic_lands_in_one_partition() {
    let topic = format!("{GTS_ID_PREFIX}cf.core.events.topic.v1~example.sdk.outbox.orders.v1");
    let first = producer_outbox_partition(&topic, "tenant-a", 16);
    for _ in 0..100 {
        assert_eq!(producer_outbox_partition(&topic, "tenant-a", 16), first);
    }
}

#[test]
fn placement_is_within_bounds() {
    for count in [1_u32, 2, 16, 64] {
        for key in ["foo", "bar", ""] {
            assert!(producer_outbox_partition("topic", key, count) < count);
        }
    }
}
