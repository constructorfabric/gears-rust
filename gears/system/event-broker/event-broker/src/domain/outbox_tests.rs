//! Placing an event in an ingest outbox partition by topic and partition key.
//!
//! Two properties matter: every event of one key of one topic lands in one
//! partition (the outbox sequences within a partition, which is the only thing
//! preserving that key's order on the way to the backend), and keys spread
//! over every partition.

use toolkit_gts::GtsInstanceId;

use super::outbox::{INGEST_OUTBOX_PARTITIONS, OutboxId, fence_key, outbox_partition_for};

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new("gts.cf.core.events.topic.v1~example.eb.outbox.topic.v1")
        .expect("valid id")
}

#[test]
fn placement_vectors_are_pinned() {
    // (murmur3_x86_32("{topic}:{key}", 0) & 0x7FFF_FFFF) % 4
    assert_eq!(outbox_partition_for(&topic(), "key-0"), 1);
    assert_eq!(outbox_partition_for(&topic(), "key-1"), 3);
    assert_eq!(outbox_partition_for(&topic(), "key-2"), 2);
}

#[test]
fn every_outbox_partition_is_reachable() {
    let reached: std::collections::BTreeSet<u32> = (0..64)
        .map(|n| outbox_partition_for(&topic(), &format!("key-{n}")))
        .collect();
    let expected: std::collections::BTreeSet<u32> =
        (0..u32::from(INGEST_OUTBOX_PARTITIONS)).collect();
    assert_eq!(reached, expected);
}

#[test]
fn one_key_always_lands_in_one_partition() {
    let first = outbox_partition_for(&topic(), "tenant-a");
    for _ in 0..64 {
        assert_eq!(outbox_partition_for(&topic(), "tenant-a"), first);
    }
}

#[test]
fn fence_key_keeps_both_halves() {
    let outbox = OutboxId::new(0x0123_4567_89ab_cdef);
    assert_eq!(
        fence_key(outbox, 7).value(),
        0x0123_4567_89ab_cdef_0000_0000_0000_0007
    );
    assert_ne!(fence_key(outbox, 7), fence_key(outbox, 8));
    assert_ne!(fence_key(outbox, 7), fence_key(OutboxId::new(1), 7));
}

#[test]
fn fence_key_keeps_a_negative_partition_id_apart() {
    let outbox = OutboxId::new(1);
    assert_eq!(
        fence_key(outbox, -1).value(),
        0x0000_0000_0000_0001_ffff_ffff_ffff_ffff
    );
    assert_ne!(fence_key(outbox, -1), fence_key(outbox, 1));
}

#[test]
fn outbox_id_round_trips_through_signed() {
    let id = OutboxId::new(u64::MAX - 5);
    assert_eq!(OutboxId::from_signed(id.signed()), id);
}
