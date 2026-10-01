use event_broker_sdk::backend::Partition;
use uuid::Uuid;

use crate::names::{
    MAX_PARTITIONS, head_payload, occurred_index, parent, parse_head_payload, segment,
};

const P3: Partition = Partition::new(3);
const TOPIC: Uuid = Uuid::from_u128(0x3f2a_9c1e_0b7d_4e21_a8c6_5f0d_9e14_b7a3);

#[test]
fn names_follow_the_design() {
    assert_eq!(
        parent(TOPIC, P3),
        "evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3"
    );
    assert_eq!(
        segment(TOPIC, P3, 1),
        "evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3_s0000000000000001"
    );
    assert_eq!(
        occurred_index(TOPIC, P3),
        "idx_evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3__occurred"
    );
}

#[test]
fn every_name_fits_the_identifier_limit_at_the_largest_partition() {
    assert_eq!(
        segment(TOPIC, Partition::new(MAX_PARTITIONS), i64::MAX).len(),
        62
    );
    assert_eq!(
        occurred_index(TOPIC, Partition::new(MAX_PARTITIONS)).len(),
        58
    );
}

#[test]
fn a_head_payload_round_trips() {
    let payload = head_payload(TOPIC, P3, 42);
    assert_eq!(payload, "3f2a9c1e0b7d4e21a8c65f0d9e14b7a3:3:42");
    assert_eq!(parse_head_payload(&payload), Some((TOPIC, P3, 42)));
    assert_eq!(parse_head_payload("nonsense"), None);
}
