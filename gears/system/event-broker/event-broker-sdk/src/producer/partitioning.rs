use toolkit_stable_hash::murmur3_x86_32;

/// The producer outbox partition an event of `topic` with partition-key value
/// `key` is enqueued into: `(murmur3_x86_32("{topic}:{key}", 0) & 0x7FFFFFFF) % count`.
///
/// Every event of one key of one topic lands in one partition, so they drain in
/// enqueue order.
#[cfg(feature = "outbox")]
pub(crate) fn producer_outbox_partition(
    topic: &str,
    key: &str,
    outbox_partition_count: u32,
) -> u32 {
    let placement = format!("{topic}:{key}");
    (murmur3_x86_32(placement.as_bytes(), 0) & 0x7FFF_FFFF) % outbox_partition_count.max(1)
}
