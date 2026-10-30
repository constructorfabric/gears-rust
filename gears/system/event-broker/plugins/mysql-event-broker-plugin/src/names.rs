//! Object names (`docs/DESIGN.md` §2.2): derived from `(topic_uuid, partition,
//! first_sequence)` when a table or segment is created, then stored with its
//! catalog row. The stored name is the one used afterwards, so a change to
//! these rules leaves existing tables as they are.

use event_broker_sdk::backend::Partition;
use uuid::Uuid;

/// The most partitions a topic may have here: four decimal digits keep every
/// table name and lock name within the 64-byte limits.
pub const MAX_PARTITIONS: u32 = 9999;

/// `evbk__{topic_uuid}_p{partition}`: the range-partitioned table of one
/// partition.
pub fn table(topic: Uuid, partition: Partition) -> String {
    format!("evbk__{}_p{partition}", topic.simple())
}

/// `s{first:016x}`: one segment, a range partition of its table.
pub fn segment(first: i64) -> String {
    format!("s{first:016x}")
}

/// The named lock (`GET_LOCK`) under which the segments of one partition of
/// one database are created, sealed, expired and dropped:
/// `evbk.{database hash}.{topic_uuid}.{partition}`, at most 59 bytes, under
/// the 64 both engines allow. Named locks are server-wide, so the database
/// is part of the name.
pub fn segment_lock(database: &str, topic: Uuid, partition: Partition) -> String {
    format!(
        "evbk.{:016x}.{}.{partition}",
        fnv1a(database.as_bytes()),
        topic.simple()
    )
}

/// 64-bit FNV-1a: stable across builds and platforms, unlike the standard
/// library's hasher.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The value of a `partition_no INT` column.
pub fn partition_column(partition: Partition) -> i32 {
    i32::try_from(partition.value()).unwrap_or(i32::MAX)
}

/// Reads back a `partition_no INT` column; `None` for a negative one.
pub fn partition_from_column(column: i32) -> Option<Partition> {
    u32::try_from(column).ok().map(Partition::new)
}
