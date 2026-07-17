use crate::ids::ConsumerGroupId;

use super::core::Core;

/// v1 round-robin rebalance per USE_CASES.md §5.
///
/// For each topic in the group's effective topic set (union of all members'
/// topics), distributes partitions round-robin among members subscribed to
/// that topic, sorted by `(created_at, id)` for determinism.
///
/// - Cursor entries are created-if-absent (sticky: existing `acked`/`last_examined`
///   survive partition handoff).
/// - `GroupState.assignments` inverted map is rebuilt from scratch.
/// - `GroupState.topology_version` is incremented.
/// - Each `SubState.assigned` and `SubState.topology_version` are updated.
pub(super) fn run_rebalance(group_id: &ConsumerGroupId, core: &mut Core) {
    let group = match core.groups.get_mut(group_id) {
        Some(g) => g,
        None => return,
    };

    // Sort members by (created_at, id) for deterministic round-robin.
    let mut members: Vec<_> = group
        .members
        .iter()
        .filter_map(|sub_id| {
            core.subscriptions
                .get(sub_id)
                .map(|s| (*sub_id, s.created_at, s.topics.clone()))
        })
        .collect();
    members.sort_by_key(|(id, created_at, _)| (*created_at, id.0));

    // Compute effective topic set: union of all members' topics.
    let all_topics: std::collections::HashSet<String> = members
        .iter()
        .flat_map(|(_, _, topics)| topics.iter().cloned())
        .collect();

    // Build new assignments: (topic, partition) → sub_id.
    let mut new_assignments = std::collections::HashMap::new();

    for topic in &all_topics {
        let partition_count = match core.topics.get(topic.as_str()) {
            Some(t) => t.partitions,
            None => continue, // topic not registered in mock - skip
        };

        // Only members subscribed to this topic are eligible.
        let eligible: Vec<_> = members
            .iter()
            .filter(|(_, _, topics)| topics.contains(topic))
            .map(|(sub_id, _, _)| *sub_id)
            .collect();

        if eligible.is_empty() {
            continue;
        }

        let n = partition_count as usize;
        let s = eligible.len();

        if s >= n {
            // More members than partitions: each partition gets one member.
            for (i, sub_id) in eligible.iter().enumerate().take(n) {
                new_assignments.insert((topic.clone(), i as u32), *sub_id);
            }
        } else {
            // Distribute partitions round-robin.
            let base = n / s;
            let extra = n % s;
            let mut cursor = 0usize;
            for (i, sub_id) in eligible.iter().enumerate() {
                let count = base + if i < extra { 1 } else { 0 };
                for p in cursor..cursor + count {
                    new_assignments.insert((topic.clone(), p as u32), *sub_id);
                }
                cursor += count;
            }
        }
    }

    // Cursor entries are NOT pre-created here: a partition has no committed cursor
    // until the consumer SEEKs it (contract: an unseeded partition → PositionsNotSet).
    // Existing cursors (set by a prior SEEK) survive rebalance - they live in
    // `group.cursor` and are never cleared here, preserving stickiness across handoff.
    let group = core.groups.get_mut(group_id).unwrap();

    // Update GroupState.assignments and bump topology_version.
    group.assignments = new_assignments.clone();
    group.topology_version += 1;
    let new_tv = group.topology_version;

    // Update each SubState.assigned and topology_version.
    for (sub_id, _, _) in &members {
        let sub_assignments: Vec<(String, u32)> = new_assignments
            .iter()
            .filter(|(_, owner)| *owner == sub_id)
            .map(|((topic, partition), _)| (topic.clone(), *partition))
            .collect();
        if let Some(sub) = core.subscriptions.get_mut(sub_id) {
            sub.assigned = sub_assignments;
            sub.topology_version = new_tv;
        }
    }
}
