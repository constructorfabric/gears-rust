//! Object names (`docs/DESIGN.md` §2.2): derived from `(topic_uuid, partition,
//! first_sequence)`, never stored.

use event_broker_sdk::backend::Partition;
use uuid::Uuid;

/// The most partitions a topic may have here: four decimal digits keep every
/// name within `PostgreSQL`'s 63-byte identifier limit.
pub const MAX_PARTITIONS: u32 = 9999;

/// `evbk__{topic_uuid}_p{partition}`: the partitioned table of one partition.
pub fn parent(topic: Uuid, partition: Partition) -> String {
    format!("evbk__{}_p{partition}", topic.simple())
}

/// `evbk__{topic_uuid}_p{partition}_s{first:016x}`: one segment.
pub fn segment(topic: Uuid, partition: Partition, first: i64) -> String {
    format!("{}_s{first:016x}", parent(topic, partition))
}

/// `idx_{parent}__occurred`: the `occurred_at` index `resolve(At)` uses.
pub fn occurred_index(topic: Uuid, partition: Partition) -> String {
    format!("idx_{}__occurred", parent(topic, partition))
}

/// The value of a `partition INT` column.
pub fn partition_column(partition: Partition) -> i32 {
    i32::try_from(partition.value()).unwrap_or(i32::MAX)
}

/// Reads back a `partition INT` column; `None` for a negative one.
pub fn partition_from_column(column: i32) -> Option<Partition> {
    u32::try_from(column).ok().map(Partition::new)
}

/// The `NOTIFY` channel heads travel on.
pub const HEAD_CHANNEL: &str = "evbk__head";

/// The `NOTIFY` payload of one head: `{topic_uuid}:{partition}:{sequence}`.
pub fn head_payload(topic: Uuid, partition: Partition, sequence: i64) -> String {
    format!("{}:{partition}:{sequence}", topic.simple())
}

/// Reads back what [`head_payload`] wrote.
pub fn parse_head_payload(payload: &str) -> Option<(Uuid, Partition, i64)> {
    let mut parts = payload.splitn(3, ':');
    let topic = Uuid::parse_str(parts.next()?).ok()?;
    let partition = Partition::new(parts.next()?.parse().ok()?);
    let sequence = parts.next()?.parse().ok()?;
    Some((topic, partition, sequence))
}

/// The advisory-lock key text one partition's maintenance pass holds.
pub fn maintenance_key(topic: Uuid, partition: Partition) -> String {
    format!("evbk:{}:{partition}", topic.simple())
}
