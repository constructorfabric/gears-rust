use std::collections::HashSet;
use std::sync::Arc;

use event_broker_sdk::backend::support::partition_for;

use crate::workload::{EventFactory, KeyPools, PARTITIONS};

fn partitions_of(factory: &mut EventFactory, pool: usize) -> HashSet<u32> {
    (0..200)
        .map(|n| partition_for(&factory.build(0, n, 0, pool).key, PARTITIONS).value())
        .collect()
}

#[test]
fn every_partition_has_keys_at_the_default_key_count() {
    assert_eq!(KeyPools::new(10_000).len(), PARTITIONS as usize);
}

#[test]
fn a_pool_builds_events_for_one_partition_only() {
    let mut factory = EventFactory::new(Arc::from(""), Arc::new(KeyPools::new(10_000)));
    let mut seen = HashSet::new();
    for pool in 0..PARTITIONS as usize {
        let partitions = partitions_of(&mut factory, pool);
        assert_eq!(partitions.len(), 1, "pool {pool} spans {partitions:?}");
        seen.extend(partitions);
    }
    assert_eq!(
        seen.len(),
        PARTITIONS as usize,
        "the pools cover every partition"
    );
}

#[test]
fn pools_wrap_and_skip_partitions_without_keys() {
    let pools = KeyPools::new(3);
    assert!(pools.len() <= 3);
    let len = pools.len();
    let mut factory = EventFactory::new(Arc::from(""), Arc::new(pools));
    assert_eq!(
        partitions_of(&mut factory, 0),
        partitions_of(&mut factory, len)
    );
}
