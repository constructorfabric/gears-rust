use super::*;

#[test]
fn partitions_count() {
    assert_eq!(Partitions::of(1).count(), 1);
    assert_eq!(Partitions::of(2).count(), 2);
    assert_eq!(Partitions::of(4).count(), 4);
    assert_eq!(Partitions::of(8).count(), 8);
    assert_eq!(Partitions::of(16).count(), 16);
    assert_eq!(Partitions::of(32).count(), 32);
    assert_eq!(Partitions::of(64).count(), 64);
}
