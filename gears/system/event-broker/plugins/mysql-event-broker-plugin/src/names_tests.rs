use event_broker_sdk::backend::Partition;
use uuid::Uuid;

use crate::names::{MAX_PARTITIONS, partition_from_column, segment, segment_lock, table};

const TOPIC: Uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);

#[test]
fn tables_and_segments_are_named_from_their_keys() {
    assert_eq!(
        (table(TOPIC, Partition::new(7)), segment(1_000_001)),
        (
            "evbk__0123456789abcdef0123456789abcdef_p7".to_owned(),
            "s00000000000f4241".to_owned()
        )
    );
}

#[test]
fn names_fit_the_64_byte_limits_at_the_most_partitions() {
    let last = Partition::new(MAX_PARTITIONS - 1);
    let database = "d".repeat(64);
    assert_eq!(
        (
            table(TOPIC, last).len(),
            segment(i64::MAX).len(),
            segment_lock(&database, TOPIC, last).len()
        ),
        (44, 17, 59)
    );
}

#[test]
fn a_lock_names_the_database_and_the_partition() {
    assert_eq!(
        segment_lock("evbk", TOPIC, Partition::new(3)),
        "evbk.d582ec60840648b1.0123456789abcdef0123456789abcdef.3"
    );
    assert_ne!(
        segment_lock("evbk", TOPIC, Partition::new(3)),
        segment_lock("evbk_other", TOPIC, Partition::new(3))
    );
}

#[test]
fn a_negative_partition_column_is_no_partition() {
    assert_eq!(
        (partition_from_column(4), partition_from_column(-1)),
        (Some(Partition::new(4)), None)
    );
}
