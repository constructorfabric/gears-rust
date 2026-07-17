use toolkit_stable_hash::murmur3_x86_32;

pub(super) fn partition_for(key: &str, partition_count: u32) -> u32 {
    (murmur3_x86_32(key.as_bytes(), 0) & 0x7FFF_FFFF) % partition_count
}
