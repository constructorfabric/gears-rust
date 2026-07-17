use super::partitioning::partition_for;

#[test]
fn single_partition_is_always_zero() {
    for key in ["foo", "bar", "tenant-id", ""] {
        assert_eq!(partition_for(key, 1), 0);
    }
}

#[test]
fn partition_is_deterministic_and_within_bounds() {
    for partition_count in [1_u32, 2, 8, 16, 64] {
        for key in ["foo", "bar", "some-key", ""] {
            let partition = partition_for(key, partition_count);
            assert!(partition < partition_count);
            assert_eq!(partition_for(key, partition_count), partition);
        }
    }
}
