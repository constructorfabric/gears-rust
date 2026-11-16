//! How a key maps to a partition.

use toolkit_stable_hash::murmur3_x86_32;

use crate::backend::Partition;

/// The partition `key` lands in among `partitions` (ADR-0002):
/// `(murmur3_x86_32(key, 0) & 0x7FFF_FFFF) % partitions`.
///
/// The contract leaves the function to each backend; the plugins in this
/// repository share this one, so a topic moved between them keeps its
/// key-to-partition placement.
#[must_use]
pub fn partition_for(key: &str, partitions: u32) -> Partition {
    Partition::new((murmur3_x86_32(key.as_bytes(), 0) & 0x7FFF_FFFF) % partitions.max(1))
}
