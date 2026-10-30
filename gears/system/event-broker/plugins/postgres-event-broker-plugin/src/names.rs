//! Object names (`docs/DESIGN.md` §2.2): derived from `(topic_uuid, partition,
//! first_sequence)` when a table is created, then stored with its catalog row.
//! The stored name is the one used afterwards, so a change to these rules
//! leaves existing tables as they are.

use event_broker_sdk::backend::Partition;
use uuid::Uuid;

/// The most partitions a topic may have here: four decimal digits keep every
/// name within `PostgreSQL`'s 63-byte identifier limit.
pub const MAX_PARTITIONS: u32 = 9999;

/// `evbk__{topic_uuid}_p{partition}`: the partitioned table of one partition.
pub fn parent(topic: Uuid, partition: Partition) -> String {
    format!("evbk__{}_p{partition}", topic.simple())
}

/// `{parent}_s{first:016x}`: one segment of the partitioned table `parent`.
pub fn segment(parent: &str, first: i64) -> String {
    format!("{parent}_s{first:016x}")
}

/// `idx_{parent}__time`: the block-range index on `sequence_time` of the
/// partitioned table `parent`; each segment gets its own from PostgreSQL.
pub fn time_index(parent: &str) -> String {
    format!("idx_{parent}__time")
}

/// What a segment named by the current rules belongs to: `(topic,
/// partition, first)`; `None` for any other name. Only a table with no
/// catalog row is placed this way.
pub fn parse_segment(name: &str) -> Option<(Uuid, Partition, i64)> {
    let rest = name.strip_prefix("evbk__")?;
    let (topic, rest) = rest.split_once("_p")?;
    let (partition, first) = rest.split_once("_s")?;
    if topic.len() != 32 || first.len() != 16 || partition.is_empty() || partition.len() > 4 {
        return None;
    }
    let digits = |s: &str, radix| {
        s.chars()
            .all(|c| c.is_digit(radix) && !c.is_ascii_uppercase())
    };
    if !digits(topic, 16) || !digits(partition, 10) || !digits(first, 16) {
        return None;
    }
    Some((
        Uuid::parse_str(topic).ok()?,
        Partition::new(partition.parse().ok()?),
        i64::from_str_radix(first, 16).ok()?,
    ))
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
