use toolkit_stable_hash::murmur3_x86_32;

/// First-party local partition assignment per ADR-0002.
///
/// The caller passes `event.partition_key` if present, else `event.tenant_id`.
/// Must be ASCII.
/// Formula: `(murmur3_x86_32(key.as_bytes(), 0) & 0x7FFFFFFF) % partition_count`.
pub(crate) fn partition_for(key: &str, partition_count: u32) -> u32 {
    let h = murmur3_x86_32(key.as_bytes(), 0) & 0x7FFF_FFFF;
    h % partition_count
}

pub(crate) fn broker_partition_input(partition_key: Option<&str>, tenant_id: uuid::Uuid) -> String {
    partition_key
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| tenant_id.to_string())
}

pub(crate) fn broker_partition(key: &str, broker_partition_count: u32) -> u32 {
    partition_for(key, broker_partition_count.max(1))
}

#[cfg(feature = "outbox")]
pub(crate) fn producer_outbox_partition(
    topic: &str,
    broker_partition: u32,
    outbox_partition_count: u32,
) -> u32 {
    let key = format!("{topic}:{broker_partition}");
    let hash = murmur3_x86_32(key.as_bytes(), 0) & 0x7FFF_FFFF;
    hash % outbox_partition_count.max(1)
}
