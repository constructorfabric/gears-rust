use super::{partition_for, topic_uuid};
use crate::GtsInstanceId;
use toolkit_gts::GTS_ID_PREFIX;

#[test]
fn zero_partitions_is_one() {
    assert_eq!(partition_for("order-service", 0).value(), 0);
}

#[test]
fn topic_uuid_is_the_gts_uuid() {
    let topic = GtsInstanceId::try_new("gts.cf.core.events.topic.v1~acme.orders.x.topic.v1")
        .expect("valid id");
    let expected = ::gts::GtsId::try_new(topic.as_ref())
        .expect("valid id")
        .to_uuid();
    assert_eq!(topic_uuid(&topic), expected);
}

#[test]
fn empty_string_partitions_consistently() {
    assert_eq!(partition_for("", 16).value(), 0);
}

#[test]
fn single_char_subject() {
    assert_eq!(partition_for("a", 2).value(), 0);
}

#[test]
fn uuid_subject_partition_vectors() {
    let key = "550e8400-e29b-41d4-a716-446655440000";
    assert_eq!(partition_for(key, 16).value(), 12);
    assert_eq!(partition_for(key, 64).value(), 28);
}

#[test]
fn single_partition_always_zero() {
    for key in [
        "foo",
        "bar",
        "baz",
        "order-service",
        "some-long-key-with-words",
    ] {
        assert_eq!(partition_for(key, 1).value(), 0);
    }
}

#[test]
fn known_ascii_key_two_partitions() {
    assert_eq!(partition_for("order-service", 2).value(), 0);
}

#[test]
fn integration_fixture_vectors_are_pinned() {
    assert_eq!(
        partition_for("00000000-0000-0000-0000-000000000001", 4).value(),
        2
    );
    assert_eq!(
        partition_for("00000000-0000-0000-0000-000000000002", 4).value(),
        0
    );
    assert_eq!(partition_for("explicit-key", 4).value(), 3);
    assert_eq!(partition_for("explicit-key", 16).value(), 15);
    assert_eq!(
        partition_for(
            &format!("{GTS_ID_PREFIX}cf.core.events.topic.v1~example.sdk.outbox.orders.v1:15"),
            4,
        )
        .value(),
        0
    );
    assert_eq!(partition_for("fixture-partition-key-0-0", 2).value(), 0);
    assert_eq!(partition_for("fixture-partition-key-1-0", 2).value(), 1);
}

#[test]
fn sign_bit_mask_is_applied_before_modulo() {
    assert_eq!(
        partition_for("00000000-0000-0000-0000-000000000001", 100).value(),
        18
    );
}

#[test]
fn partition_is_within_bounds() {
    for partitions in [1_u32, 2, 16, 64, 100] {
        for key in ["foo", "bar", "some-partition-key", ""] {
            let partition = partition_for(key, partitions).value();
            assert!(partition < partitions);
        }
    }
}

#[test]
fn partition_is_deterministic() {
    let key = "repeat-test-subject";
    let first = partition_for(key, 32).value();
    for _ in 0..100 {
        assert_eq!(partition_for(key, 32).value(), first);
    }
}
