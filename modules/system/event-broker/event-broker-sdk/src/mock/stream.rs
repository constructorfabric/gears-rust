use std::time::Instant;

use tokio::time::sleep;
use uuid::Uuid;

use crate::consumer::backend::{FrameStream, PartitionSlot, WireEvent, WireFrame};
use crate::ids::SubscriptionId;

use super::core::MockBroker;

/// Open a live `FrameStream` for the given subscription.
///
/// Emits:
/// 1. Initial `Topology` frame.
/// 2. Events loop: deliver new events, emit `Heartbeat` on idle.
/// 3. `Topology` frame whenever the group's `topology_version` advances.
/// 4. Fault: `SubscriptionGone` (410) or `NotFound` (404) if injected.
pub(super) fn open_stream(broker: MockBroker, sub_id: SubscriptionId) -> FrameStream {
    let stream = async_stream::try_stream! {
        use crate::error::EventBrokerError;

        // ── 0. Emit initial Topology frame ────────────────────────────────────
        {
            let core = broker.core.lock().await;
            let sub = core.subscriptions.get(&sub_id)
                .ok_or_else(|| EventBrokerError::Internal(format!("subscription {sub_id:?} not found")))?;
            let group = core.groups.get(&sub.group);
            let topology_version = group.map(|g| g.topology_version).unwrap_or(0);
            let assigned: Vec<PartitionSlot> = sub.assigned.iter().map(|(_, p)| PartitionSlot {
                topic_ix: 0,
                partition: *p,
            }).collect();
            yield WireFrame::Topology { topology_version, assigned };
        }

        // ── 1. Stream loop ────────────────────────────────────────────────────
        loop {
            // Check fault injection first.
            {
                let faults = broker.faults.lock().await;
                if faults.force_gone.contains(&sub_id) {
                    // 410 Gone — graceful shard shutdown.
                    Err(EventBrokerError::Internal(
                        "410: Subscription terminated; re-JOIN to recover".to_owned()
                    ))?;
                }
                if faults.force_not_found.contains(&sub_id) {
                    // 404 SubscriptionNotFound.
                    Err(EventBrokerError::Internal(
                        "404: Subscription not found or expired".to_owned()
                    ))?;
                }
            }

            // Check for topology change (rebalance during active stream).
            {
                let mut core = broker.core.lock().await;
                let sub = core.subscriptions.get(&sub_id)
                    .ok_or_else(|| EventBrokerError::Internal("subscription gone".to_owned()))?;
                let group = core.groups.get(&sub.group);
                let current_tv = group.map(|g| g.topology_version).unwrap_or(0);
                if current_tv > sub.topology_version {
                    let new_assigned: Vec<PartitionSlot> = sub.assigned.iter().map(|(_, p)| PartitionSlot {
                        topic_ix: 0,
                        partition: *p,
                    }).collect();
                    // Update sub's tracked topology_version.
                    if let Some(sub_mut) = core.subscriptions.get_mut(&sub_id) {
                        sub_mut.topology_version = current_tv;
                    }
                    yield WireFrame::Topology { topology_version: current_tv, assigned: new_assigned };
                    // Continue to deliver any pending events after topology update.
                }
            }

            // Collect pending events under the lock, then yield outside it.
            let mut pending: Vec<WireFrame> = Vec::new();
            {
                let mut core = broker.core.lock().await;
                let sub = match core.subscriptions.get(&sub_id) {
                    Some(s) => s,
                    None => break,
                };
                let auto_commit = sub.auto_commit;
                let assigned = sub.assigned.clone();
                let group_id = sub.group.clone();

                for (topic, partition) in &assigned {
                    let sub = core.subscriptions.get(&sub_id).unwrap();
                    let seek_offset = sub.seek.get(&(topic.clone(), *partition)).copied();
                    let sent_offset = sub.sent.get(&(topic.clone(), *partition)).copied().unwrap_or(-1);
                    let start = seek_offset.unwrap_or(sent_offset + 1).max(0);

                    let events: Vec<_> = core.topics
                        .get(topic.as_str())
                        .map(|t| t.read(*partition, start, 100).into_iter().map(|se| se.event.clone()).collect())
                        .unwrap_or_default();

                    for stamped in events {
                        let new_sent = stamped.offset.unwrap_or(0);
                        pending.push(WireFrame::Event(WireEvent {
                            id: stamped.id,
                            type_id: stamped.type_id.clone(),
                            topic: stamped.topic.clone(),
                            tenant_id: stamped.tenant_id,
                            subject: stamped.subject.clone(),
                            subject_type: stamped.subject_type.clone(),
                            partition: stamped.partition.unwrap_or(*partition),
                            sequence: stamped.sequence.unwrap_or(0),
                            offset: stamped.offset.unwrap_or(0),
                            occurred_at: stamped.occurred_at,
                            sequence_time: stamped.sequence_time.unwrap_or_else(chrono::Utc::now),
                            trace_parent: stamped.trace_parent.clone(),
                            data: stamped.data.clone().unwrap_or(serde_json::Value::Null),
                        }));
                        // Advance `sent` (and `acked` for auto_commit) under lock.
                        if let Some(sub_mut) = core.subscriptions.get_mut(&sub_id) {
                            sub_mut.sent.insert((topic.clone(), *partition), new_sent);
                            if auto_commit {
                                if let Some(group) = core.groups.get_mut(&group_id) {
                                    let entry = group.cursor.entry((topic.clone(), *partition)).or_default();
                                    entry.acked = entry.acked.max(new_sent);
                                }
                            }
                        }
                    }
                }
                // Lock released here (end of block).
            }
            let delivered_any = !pending.is_empty();

            // Yield collected frames outside the lock.
            for frame in pending {
                yield frame;
            }

            if !delivered_any {
                // Idle — wait for a new event or heartbeat timeout.
                let heartbeat_interval = {
                    let faults = broker.faults.lock().await;
                    faults.heartbeat_interval
                };

                tokio::select! {
                    _ = broker.notify.notified() => {
                        // New event published or topology changed — loop again.
                    }
                    _ = sleep(heartbeat_interval) => {
                        yield WireFrame::Heartbeat;
                    }
                }
            }
        }
    };

    Box::pin(stream)
}
