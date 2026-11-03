//! How a key maps to a partition, and how a topic is named inside a medium.

use toolkit_stable_hash::murmur3_x86_32;
use uuid::Uuid;

use crate::GtsInstanceId;
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

/// The fixed-width name a backend stores a topic under: `GtsId::to_uuid`, a
/// UUID v5 of the identifier, the same derivation the gear's cluster keys use.
#[must_use]
pub fn topic_uuid(topic: &GtsInstanceId) -> Uuid {
    match ::gts::GtsId::try_new(topic.as_ref()) {
        Ok(id) => id.to_uuid(),
        // A `GtsInstanceId` is validated on construction, so this is
        // unreachable in practice; it still has to be deterministic.
        Err(_) => Uuid::new_v5(&Uuid::NAMESPACE_URL, topic.as_ref().as_bytes()),
    }
}
