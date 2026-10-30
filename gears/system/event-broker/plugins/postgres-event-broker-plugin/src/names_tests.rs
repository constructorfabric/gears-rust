use event_broker_sdk::backend::Partition;
use uuid::Uuid;

use crate::names::{
    MAX_PARTITIONS, head_payload, parent, parse_head_payload, parse_segment, segment, time_index,
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
        segment(&parent(TOPIC, P3), 1),
        "evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3_s0000000000000001"
    );
    assert_eq!(
        time_index(&parent(TOPIC, P3)),
        "idx_evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3__time"
    );
}

#[test]
fn every_name_fits_the_identifier_limit_at_the_largest_partition() {
    assert_eq!(
        segment(&parent(TOPIC, Partition::new(MAX_PARTITIONS)), i64::MAX).len(),
        62
    );
    assert_eq!(
        time_index(&parent(TOPIC, Partition::new(MAX_PARTITIONS))).len(),
        54
    );
}

#[test]
fn a_head_payload_round_trips() {
    let payload = head_payload(TOPIC, P3, 42);
    assert_eq!(payload, "3f2a9c1e0b7d4e21a8c65f0d9e14b7a3:3:42");
    assert_eq!(parse_head_payload(&payload), Some((TOPIC, P3, 42)));
    assert_eq!(parse_head_payload("nonsense"), None);
}

#[test]
fn a_segment_name_is_placed_back() {
    let names = [
        segment(&parent(TOPIC, P3), 0x3e9),
        "evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3".to_owned(),
        "evbk__3F2A9C1E0B7D4E21A8C65F0D9E14B7A3_p3_s00000000000003e9".to_owned(),
        "evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p_s00000000000003e9".to_owned(),
        "evbk__3f2a9c1e0b7d4e21a8c65f0d9e14b7a3_p3_s3e9".to_owned(),
        "evbk__partition".to_owned(),
    ];
    assert_eq!(
        names.map(|name| parse_segment(&name)),
        [Some((TOPIC, P3, 0x3e9)), None, None, None, None, None]
    );
}
