use toolkit_db::outbox::Partitions;
use uuid::Uuid;

use super::partition_of;

#[test]
fn a_partition_is_in_range_and_stable() {
    for count in [1_u16, 2, 16, 64] {
        for n in 0..100_u128 {
            let id = Uuid::from_u128(n << 96 | n);
            let partition = partition_of(id, Partitions::of(count));
            assert!(partition < u32::from(count));
            assert_eq!(partition, partition_of(id, Partitions::of(count)));
        }
    }
}
