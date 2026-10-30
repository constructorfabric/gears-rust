//! Placing an event in an ingest outbox partition by topic and partition key.
//!
//! Two properties matter: every event of one key of one topic lands in one
//! partition (the outbox sequences within a partition, which is the only thing
//! preserving that key's order on the way to the backend), and keys spread
//! over every partition.

use toolkit_gts::GtsInstanceId;

use super::outbox::{OutboxId, OutboxPartitions, fence_key, outbox_partition_for};

fn four() -> OutboxPartitions {
    OutboxPartitions::new(4).expect("a power of 2 within 64")
}

fn topic() -> GtsInstanceId {
    GtsInstanceId::try_new("gts.cf.core.events.topic.v1~example.eb.outbox.topic.v1")
        .expect("valid id")
}

#[test]
fn placement_vectors_are_pinned() {
    // (murmur3_x86_32("{topic}:{key}", 0) & 0x7FFF_FFFF) % 4
    assert_eq!(outbox_partition_for(&topic(), "key-0", four()), 1);
    assert_eq!(outbox_partition_for(&topic(), "key-1", four()), 3);
    assert_eq!(outbox_partition_for(&topic(), "key-2", four()), 2);
    // ... % 16, the default group's count.
    assert_eq!(
        [0, 1, 2].map(|n| outbox_partition_for(
            &topic(),
            &format!("key-{n}"),
            OutboxPartitions::DEFAULT
        )),
        [5, 11, 2]
    );
}

#[test]
fn every_outbox_partition_is_reachable() {
    for partitions in [four(), OutboxPartitions::DEFAULT] {
        let reached: std::collections::BTreeSet<u32> = (0..256)
            .map(|n| outbox_partition_for(&topic(), &format!("key-{n}"), partitions))
            .collect();
        let expected: std::collections::BTreeSet<u32> = (0..u32::from(partitions.get())).collect();
        assert_eq!(reached, expected);
    }
}

#[test]
fn one_key_always_lands_in_one_partition() {
    let first = outbox_partition_for(&topic(), "tenant-a", OutboxPartitions::DEFAULT);
    for _ in 0..64 {
        assert_eq!(
            outbox_partition_for(&topic(), "tenant-a", OutboxPartitions::DEFAULT),
            first
        );
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
fn outbox_id_round_trips_through_its_16_hex_digits() {
    for id in [
        OutboxId::new(0),
        OutboxId::new(0x0123_4567_89ab_cdef),
        OutboxId::new(u64::MAX),
    ] {
        assert_eq!(OutboxId::parse(&id.to_string()), Some(id));
    }
    assert_eq!(
        OutboxId::new(0x0123_4567_89ab_cdef).to_string(),
        "0123456789abcdef"
    );
}

/// Only the exact form this gear writes is read back: it becomes part of a
/// fence key, so anything else is not an identity this gear minted.
#[test]
fn outbox_id_refuses_any_other_shape() {
    for stored in [
        "",
        "123",
        "0123456789ABCDEF",
        "0123456789abcdef0",
        "+123456789abcdef",
        "0123456789abcdeg",
    ] {
        assert_eq!(OutboxId::parse(stored), None, "{stored:?}");
    }
}

/// C8: a power of 2 from 1 to 64; absent, 16.
#[test]
fn outbox_partitions_are_a_power_of_two_from_1_to_64() {
    for count in [1, 2, 4, 8, 16, 32, 64] {
        assert_eq!(
            OutboxPartitions::new(count).map(OutboxPartitions::get),
            Some(u16::try_from(count).expect("small"))
        );
    }
    for count in [0, 3, 12, 65, 128, u32::MAX] {
        assert_eq!(OutboxPartitions::new(count), None, "{count}");
    }
    assert_eq!(OutboxPartitions::default().get(), 16);
}
