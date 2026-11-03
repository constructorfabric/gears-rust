use event_broker_sdk::Sequence;
use event_broker_sdk::backend::FenceKey;
use uuid::Uuid;

use crate::names;

const TOPIC: Uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);

#[test]
fn names_carry_the_prefix() {
    assert_eq!(
        [
            names::event_topic("evbk", TOPIC),
            names::logs_topic("evbk"),
            names::fences_topic("evbk"),
            names::log_transaction("evbk", TOPIC),
            names::fence_transaction("evbk", FenceKey::new(0xabc)),
        ],
        [
            "evbk.0123456789abcdef0123456789abcdef",
            "evbk.logs",
            "evbk.fences",
            "evbk.log.0123456789abcdef0123456789abcdef",
            "evbk.fence.00000000000000000000000000000abc",
        ]
    );
}

#[test]
fn a_fence_key_maps_to_its_partition_by_its_value() {
    assert_eq!(
        [
            names::fence_partition(FenceKey::new(0), 16),
            names::fence_partition(FenceKey::new(17), 16),
            names::fence_partition(FenceKey::new(u128::MAX), 16),
            names::fence_partition(FenceKey::new(5), 0),
        ],
        [0, 1, 15, 0]
    );
}

#[test]
fn a_sequence_is_its_offset_plus_one() {
    assert_eq!(
        [names::sequence_of(0), names::sequence_of(41)],
        [Sequence::assigned(1), Sequence::assigned(42)]
    );
    assert_eq!(
        [
            names::first_offset_after(Sequence::NONE),
            names::first_offset_after(Sequence::assigned(42)),
        ],
        [0, 42]
    );
}
