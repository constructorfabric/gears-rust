use std::collections::HashMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::ResolvedPosition;
use crate::api::{
    AssignedPartition, EventBroker, IngestOutcome, JoinRequest, ProducerCursor, ProducerMode,
    SeekResult, SubscriptionAssignment,
};
use crate::api::{FrameStream, SeekPosition};
use crate::error::EventBrokerError;
use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use crate::models::Event;
use crate::models::{
    ConsumerGroup, ConsumerGroupKind, ConsumerGroupQuery, CreateConsumerGroupRequest, EventType,
    Page, PartitionRange, ResetScope, Subscription, Topic, TopicSegment,
};

use super::core::{Core, GroupReg, GroupState, MockBroker, SubState};
use super::ingest::{ingest_batch, ingest_one};
use super::rebalance::run_rebalance;
use super::stream::open_stream;

// --- Helper --------------------------------------------------------------------

fn principal(ctx: &SecurityContext) -> String {
    ctx.subject_id().to_string()
}

fn tenant(ctx: &SecurityContext) -> Uuid {
    ctx.subject_tenant_id()
}

fn instant_to_utc(instant: Instant) -> DateTime<Utc> {
    let now_instant = Instant::now();
    let now_utc = Utc::now();
    if instant >= now_instant {
        now_utc
            + chrono::Duration::from_std(instant.duration_since(now_instant)).unwrap_or_default()
    } else {
        now_utc
            - chrono::Duration::from_std(now_instant.duration_since(instant)).unwrap_or_default()
    }
}

/// Batch publish limits (DESIGN: ingest batch caps).
const MAX_BATCH_EVENTS: usize = 100;
const MAX_BATCH_BYTES: usize = 1024 * 1024; // 1 MiB total payload

/// Reject a batch exceeding the event-count or total-payload-byte cap.
fn check_batch_size(events: &[Event]) -> Result<(), EventBrokerError> {
    let count = events.len();
    let bytes: usize = events
        .iter()
        .map(|e| {
            e.data
                .as_ref()
                .map(|d| serde_json::to_vec(d).map(|v| v.len()).unwrap_or(0))
                .unwrap_or(0)
        })
        .sum();
    if count > MAX_BATCH_EVENTS || bytes > MAX_BATCH_BYTES {
        return Err(EventBrokerError::BatchTooLarge {
            count,
            bytes,
            max_count: MAX_BATCH_EVENTS,
            max_bytes: MAX_BATCH_BYTES,
            detail: format!(
                "batch of {count} events / {bytes} bytes exceeds limits ({MAX_BATCH_EVENTS} events, {MAX_BATCH_BYTES} bytes)"
            ),
            instance: String::new(),
        });
    }
    Ok(())
}

/// Resolve `AtTimestamp` against the in-memory log by a linear scan over stored
/// events ordered by offset (offset == storage order == `occurred_at` order for
/// the mock's append-only log). Returns the offset of the first event whose
/// `occurred_at >= ts`. A timestamp at/before the retention floor resolves to
/// the floor offset; a timestamp beyond the high-water mark resolves to the HWM.
fn resolve_at_timestamp(core: &Core, topic: &str, partition: u32, ts: &str) -> i64 {
    let topic_state = match core.topics.get(topic) {
        Some(t) => t,
        None => return 0,
    };
    // High-water mark = last assigned offset (next_offset - 1), floored at 0.
    let hwm = topic_state
        .next_offset
        .get(&partition)
        .copied()
        .unwrap_or(0)
        .saturating_sub(1)
        .max(0);
    let log = match topic_state.log.get(&partition) {
        Some(l) if !l.is_empty() => l,
        _ => return hwm,
    };

    // Parse the requested timestamp; an unparseable timestamp falls back to the
    // retention floor (the earliest stored offset).
    let target = match chrono::DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => {
            return log
                .first()
                .and_then(|e| e.event.offset)
                .map(|o| o - 1)
                .unwrap_or(0);
        }
    };

    // Cursor is last-processed (emit from cursor+1). To DELIVER the first event at
    // or after `target`, the cursor is that event's offset minus 1.
    // Timestamp at/before the retention floor → clamp to floor (deliver the first event).
    let floor_offset = log.first().and_then(|e| e.event.offset).unwrap_or(1);
    if let Some(first) = log.first()
        && target <= first.event.occurred_at
    {
        return floor_offset - 1;
    }

    // Linear scan for the first event with occurred_at >= target.
    for stored in log {
        if stored.event.occurred_at >= target {
            return stored.event.offset.unwrap_or(floor_offset) - 1;
        }
    }

    // Timestamp beyond the last stored event → high-water mark (equivalent to Latest:
    // cursor = last existing offset, emit only future events).
    hwm
}

impl MockBroker {
    /// Honour the `reject_persist` fault (M3 chain-gap surface).
    async fn check_reject_persist(&self) -> Result<(), EventBrokerError> {
        let faults = self.faults.lock().await;
        if let Some(reason) = &faults.reject_persist {
            return Err(EventBrokerError::Internal(reason.clone()));
        }
        Ok(())
    }

    /// Charge `n` units against the producer publish rate-limit allowance. When
    /// the allowance is insufficient, refuse with `RateLimited` and do not
    /// consume any allowance.
    async fn consume_publish_allowance(&self, n: u32) -> Result<(), EventBrokerError> {
        let mut faults = self.faults.lock().await;
        if let Some(remaining) = faults.publish_rate_limit {
            if remaining < n {
                return Err(EventBrokerError::RateLimited {
                    retry_after_secs: 30,
                    detail: format!(
                        "publish rate limit exhausted ({remaining} of {n} requested unit(s) available)"
                    ),
                    instance: String::new(),
                });
            }
            faults.publish_rate_limit = Some(remaining - n);
        }
        Ok(())
    }
}

#[async_trait]
impl EventBroker for MockBroker {
    // -- Producer --------------------------------------------------------------
    async fn register_producer(
        &self,
        _ctx: &SecurityContext,
        mode: ProducerMode,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        let id = ProducerId(Uuid::new_v4());
        let mut core = self.core.lock().await;
        core.producers.insert(id, super::core::ProducerReg { mode });
        let _ = client_agent; // stored for logs in production; no-op in mock
        Ok(id)
    }

    async fn publish(
        &self,
        _ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.check_reject_persist().await?;
        self.consume_publish_allowance(1).await?;
        let mut core = self.core.lock().await;
        let (outcome, _) = ingest_one(&mut core, event)?;
        if outcome == IngestOutcome::Accepted {
            self.notify.notify_waiters();
        }
        Ok(outcome)
    }

    async fn publish_sync(
        &self,
        _ctx: &SecurityContext,
        event: &Event,
    ) -> Result<IngestOutcome, EventBrokerError> {
        self.check_reject_persist().await?;
        self.consume_publish_allowance(1).await?;
        // The mock's `ingest_one` appends to the in-memory log synchronously, so
        // by the time it returns the event is durably "persisted". Report the
        // persist-confirmed outcome (Accepted → Persisted; Duplicate stays).
        let mut core = self.core.lock().await;
        let (outcome, _) = ingest_one(&mut core, event)?;
        let outcome = match outcome {
            IngestOutcome::Accepted => IngestOutcome::Persisted,
            other => other,
        };
        if outcome == IngestOutcome::Persisted {
            self.notify.notify_waiters();
        }
        Ok(outcome)
    }

    async fn publish_batch(
        &self,
        _ctx: &SecurityContext,
        events: &[Event],
    ) -> Result<Vec<IngestOutcome>, EventBrokerError> {
        self.check_reject_persist().await?;
        check_batch_size(events)?;
        self.consume_publish_allowance(events.len() as u32).await?;
        let mut core = self.core.lock().await;
        let results = ingest_batch(&mut core, events)?;
        let any_accepted = results.iter().any(|(o, _)| *o == IngestOutcome::Accepted);
        if any_accepted {
            self.notify.notify_waiters();
        }
        Ok(results.into_iter().map(|(o, _)| o).collect())
    }

    async fn get_producer_cursors(
        &self,
        _ctx: &SecurityContext,
        producer_id: ProducerId,
    ) -> Result<Vec<ProducerCursor>, EventBrokerError> {
        let core = self.core.lock().await;
        let cursors = core
            .producer_state
            .iter()
            .filter(|((pid, _, _), _)| *pid == producer_id)
            .map(|((_, topic, partition), seq)| ProducerCursor {
                topic: topic.clone(),
                partition: *partition,
                last_sequence: *seq,
            })
            .collect();
        Ok(cursors)
    }

    async fn reset_producer_chain(
        &self,
        _ctx: &SecurityContext,
        producer_id: ProducerId,
        scope: ResetScope<'_>,
    ) -> Result<(), EventBrokerError> {
        let mut core = self.core.lock().await;
        match scope {
            ResetScope::Partition { topic, partition } => {
                // Reset single (producer, topic, partition).
                core.producer_state
                    .remove(&(producer_id, topic.to_owned(), partition));
            }
            ResetScope::Topic(topic) => {
                // Reset all (producer, topic, *) - M7 branch 2.
                let keys: Vec<_> = core
                    .producer_state
                    .keys()
                    .filter(|(pid, tp, _)| *pid == producer_id && tp == topic)
                    .cloned()
                    .collect();
                for k in keys {
                    core.producer_state.remove(&k);
                }
            }
            ResetScope::AllTopics => {
                // Reset all (producer, *, *) - M7 branch 1 (reset-all).
                let keys: Vec<_> = core
                    .producer_state
                    .keys()
                    .filter(|(pid, _, _)| *pid == producer_id)
                    .cloned()
                    .collect();
                for k in keys {
                    core.producer_state.remove(&k);
                }
            }
        }
        Ok(())
    }

    // -- Consumer groups -------------------------------------------------------
    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        // B3: client_agent must be ASCII, 1-256 bytes.
        if req.client_agent.is_empty()
            || req.client_agent.len() > 256
            || !req.client_agent.is_ascii()
        {
            return Err(EventBrokerError::InvalidEventField {
                field: "client_agent",
                detail: "client_agent must be ASCII and 1-256 bytes".to_owned(),
                instance: "/v1/consumer-groups".to_owned(),
            });
        }
        let group_id = ConsumerGroupId::new(Uuid::new_v4());
        let mut core = self.core.lock().await;
        core.groups_registry.insert(
            group_id,
            GroupReg {
                kind: ConsumerGroupKind::Anonymous,
                owner_tenant: tenant(ctx),
                owner_principal: principal(ctx),
            },
        );
        Ok(ConsumerGroup {
            id: group_id,
            tenant_id: tenant(ctx),
            owner_principal_id: principal(ctx),
            kind: ConsumerGroupKind::Anonymous,
            description: None,
            created_at: Utc::now(),
        })
    }

    async fn get_consumer_group(
        &self,
        _ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        let core = self.core.lock().await;
        let reg = core.groups_registry.get(id).ok_or_else(|| {
            EventBrokerError::ConsumerGroupNotFound {
                group_id: *id,
                detail: format!("consumer group '{id}' not found"),
                instance: String::new(),
            }
        })?;
        Ok(ConsumerGroup {
            id: *id,
            tenant_id: reg.owner_tenant,
            owner_principal_id: reg.owner_principal.clone(),
            kind: reg.kind,
            description: None,
            created_at: Utc::now(),
        })
    }

    async fn list_consumer_groups(
        &self,
        _ctx: &SecurityContext,
        query: ConsumerGroupQuery,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        // Mock: cursor, filter, and orderby are accepted but not implemented.
        // Returns the first page only.
        let page_limit = query.limit.unwrap_or(25) as usize;
        let core = self.core.lock().await;
        let items: Vec<ConsumerGroup> = core
            .groups_registry
            .iter()
            .take(page_limit)
            .map(|(id, reg)| ConsumerGroup {
                id: *id,
                tenant_id: reg.owner_tenant,
                owner_principal_id: reg.owner_principal.clone(),
                kind: reg.kind,
                description: None,
                created_at: Utc::now(),
            })
            .collect();
        let total = core.groups_registry.len();
        let next_cursor = if total > page_limit {
            Some("mock-next-cursor".to_owned())
        } else {
            None
        };
        Ok(Page {
            items,
            next_cursor,
            prev_cursor: None,
            limit: page_limit as u32,
        })
    }

    async fn delete_consumer_group(
        &self,
        _ctx: &SecurityContext,
        id: &ConsumerGroupId,
    ) -> Result<(), EventBrokerError> {
        let mut core = self.core.lock().await;
        if !core.groups_registry.contains_key(id) {
            return Err(EventBrokerError::ConsumerGroupNotFound {
                group_id: *id,
                detail: format!("consumer group '{id}' not found"),
                instance: String::new(),
            });
        }
        if core.groups.contains_key(id) {
            let has_members = core
                .groups
                .get(id)
                .map(|g| !g.members.is_empty())
                .unwrap_or(false);
            if has_members {
                return Err(EventBrokerError::ConsumerGroupHasActiveMembers {
                    detail: format!("consumer group '{id}' has active subscriptions"),
                    instance: String::new(),
                });
            }
        }
        core.groups_registry.remove(id);
        core.groups.remove(id);
        Ok(())
    }

    // -- Subscriptions ---------------------------------------------------------
    async fn join(
        &self,
        _ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        // B3: client_agent must be ASCII, 1-256 bytes (RFC 9110 User-Agent grammar).
        if req.client_agent.is_empty()
            || req.client_agent.len() > 256
            || !req.client_agent.is_ascii()
        {
            return Err(EventBrokerError::InvalidEventField {
                field: "client_agent",
                detail: "client_agent must be ASCII and 1-256 bytes".to_owned(),
                instance: "/v1/subscriptions".to_owned(),
            });
        }
        // B4: a subscription carries 1-64 interests.
        const MAX_INTERESTS: usize = 64;
        if req.interests.is_empty() || req.interests.len() > MAX_INTERESTS {
            return Err(EventBrokerError::InvalidEventField {
                field: "interests",
                detail: format!(
                    "interests must be 1..={MAX_INTERESTS} (got {})",
                    req.interests.len()
                ),
                instance: "/v1/subscriptions".to_owned(),
            });
        }
        let now = Instant::now();
        let sub_id = SubscriptionId(Uuid::new_v4());
        let timeout = req
            .session_timeout
            .unwrap_or(std::time::Duration::from_secs(30));

        let mut core = self.core.lock().await;

        // Validate group exists.
        if !core.groups_registry.contains_key(&req.group) {
            return Err(EventBrokerError::ConsumerGroupNotFound {
                group_id: req.group,
                detail: format!("consumer group '{}' not registered", req.group),
                instance: String::new(),
            });
        }

        // Build the set of topics from interests.
        let topics: std::collections::HashSet<String> =
            req.interests.iter().map(|i| i.topic.clone()).collect();

        // Capacity guard: a group can carry at most one active member per partition
        // for any given topic (v1 round-robin). If every topic this member is
        // interested in is already saturated (active members ≥ partitions), the
        // member could never be assigned a partition - refuse the JOIN.
        let active_members = core
            .groups
            .get(&req.group)
            .map(|g| g.members.len() as u32)
            .unwrap_or(0);
        let max_partitions = topics
            .iter()
            .filter_map(|t| core.topics.get(t).map(|ts| ts.partitions))
            .max();
        if let Some(partitions) = max_partitions
            && active_members >= partitions
        {
            return Err(EventBrokerError::GroupAtCapacity {
                active: active_members,
                partitions,
                detail: format!(
                    "consumer group '{}' already has {active_members} active member(s) for a topic with {partitions} partition(s); no partition available for a further member",
                    req.group
                ),
                instance: String::new(),
            });
        }

        // Build SubState.
        let sub = SubState {
            group: req.group,
            interests: req.interests,
            topics,
            assigned: Vec::new(),
            topology_version: 0,
            created_at: now,
            session_timeout: timeout,
            expires_at: now + timeout,
            seek: HashMap::new(),
            sent: HashMap::new(),
            scanned: HashMap::new(),
            terminated: false,
        };
        core.subscriptions.insert(sub_id, sub);

        // Ensure GroupState exists.
        core.groups.entry(req.group).or_insert_with(GroupState::new);
        let group = core.groups.get_mut(&req.group).unwrap();
        group.members.push(sub_id);

        // Run v1 rebalance.
        run_rebalance(&req.group, &mut core);
        self.notify.notify_waiters();

        // Build SubscriptionAssignment from the group's cursor state.
        let group = core.groups.get(&req.group).unwrap();
        let sub = core.subscriptions.get(&sub_id).unwrap();
        let topology_version = group.topology_version;
        let assigned: Vec<AssignedPartition> = sub
            .assigned
            .iter()
            .map(|(topic, partition)| AssignedPartition {
                topic: topic.clone(),
                partition: *partition,
            })
            .collect();

        Ok(SubscriptionAssignment {
            subscription_id: sub_id,
            topology_version,
            expires_at: instant_to_utc(sub.expires_at),
            assigned,
        })
    }

    async fn get_subscription(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<Subscription, EventBrokerError> {
        let core = self.core.lock().await;
        let sub = core
            .subscriptions
            .get(&id)
            .ok_or_else(|| EventBrokerError::Internal(format!("subscription {id:?} not found")))?;
        let group = core.groups.get(&sub.group);
        let topology_version = group.map(|g| g.topology_version).unwrap_or(0);
        Ok(Subscription {
            id,
            consumer_group: sub.group,
            assigned: sub
                .assigned
                .iter()
                .map(|(_, p)| crate::models::PartitionAssignment {
                    topic_ix: 0,
                    partition: *p,
                })
                .collect(),
            topology_version,
            expires_at: instant_to_utc(sub.expires_at),
        })
    }

    async fn list_subscriptions(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<Subscription>, EventBrokerError> {
        let core = self.core.lock().await;
        Ok(core
            .subscriptions
            .iter()
            .map(|(id, sub)| {
                let tv = core
                    .groups
                    .get(&sub.group)
                    .map(|g| g.topology_version)
                    .unwrap_or(0);
                Subscription {
                    id: *id,
                    consumer_group: sub.group,
                    assigned: sub
                        .assigned
                        .iter()
                        .map(|(_, p)| crate::models::PartitionAssignment {
                            topic_ix: 0,
                            partition: *p,
                        })
                        .collect(),
                    topology_version: tv,
                    expires_at: instant_to_utc(sub.expires_at),
                }
            })
            .collect())
    }

    async fn leave(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<(), EventBrokerError> {
        let mut core = self.core.lock().await;
        match core.subscriptions.remove(&id) {
            Some(sub) => {
                let group_id = sub.group;
                if let Some(group) = core.groups.get_mut(&group_id) {
                    group.members.retain(|m| *m != id);
                }
                run_rebalance(&group_id, &mut core);
                self.notify.notify_waiters();
                Ok(())
            }
            // B2: leaving an unknown/expired subscription is a 404, not a silent no-op.
            None => Err(EventBrokerError::SubscriptionNotFound {
                id,
                detail: "no such subscription (unknown or already removed)".to_owned(),
                instance: format!("/v1/subscriptions/{id:?}"),
            }),
        }
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        // A subscription terminated by a gain / lose-all rebalance is dead - any
        // reuse of its id returns 410 (the safety net for a consumer that missed
        // the terminal control frame).
        {
            let mut core = self.core.lock().await;
            let sub = core.subscriptions.get(&id).ok_or_else(|| {
                EventBrokerError::SubscriptionNotFound {
                    id,
                    detail: "no such subscription (unknown or expired)".to_owned(),
                    instance: format!("/v1/events:stream?subscription_id={id:?}"),
                }
            })?;
            if sub.terminated {
                return Err(EventBrokerError::Internal(
                    "410: Subscription terminated; re-JOIN to recover".to_owned(),
                ));
            }
            // A1: every assigned partition must have a committed cursor (set via SEEK,
            // or inherited group-scoped from a prior member) before the stream opens.
            let group_cursors = core.groups.get(&sub.group).map(|g| &g.cursor);
            let unseeded: Vec<(String, u32)> = sub
                .assigned
                .iter()
                .filter(|(t, p)| {
                    group_cursors
                        .map(|c| !c.contains_key(&(t.clone(), *p)))
                        .unwrap_or(true)
                })
                .cloned()
                .collect();
            if !unseeded.is_empty() {
                return Err(EventBrokerError::PositionsNotSet {
                    unseeded,
                    detail: "SEEK every assigned partition before opening the stream".to_owned(),
                    instance: format!("/v1/events:stream?subscription_id={id:?}"),
                });
            }
            if let Some(sub) = core.subscriptions.get_mut(&id) {
                sub.expires_at = Instant::now() + sub.session_timeout;
            }
        }
        // A2: one open stream per subscription. Mark it streaming; the stream's
        // Drop guard clears the marker when it ends/drops.
        {
            let mut streaming = self.streaming.lock().unwrap();
            if streaming.contains(&id) {
                return Err(EventBrokerError::StreamingInProgress {
                    detail: "a stream is already open for this subscription".to_owned(),
                    instance: format!("/v1/events:stream?subscription_id={id:?}"),
                });
            }
            streaming.insert(id);
        }
        Ok(open_stream(self.clone(), id))
    }

    async fn seek(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        // A2: SEEK is a pre-stream operation - rejected while a stream is open.
        if self.streaming.lock().unwrap().contains(&id) {
            return Err(EventBrokerError::StreamingInProgress {
                detail: "SEEK is not allowed while a stream is open; it is a pre-stream operation"
                    .to_owned(),
                instance: format!("/v1/subscriptions/{id:?}:seek"),
            });
        }
        let mut core = self.core.lock().await;
        // A3: SEEK is only valid for partitions assigned to this subscription.
        let assigned: Vec<(String, u32)> = core
            .subscriptions
            .get(&id)
            .map(|s| s.assigned.clone())
            .unwrap_or_default();
        for pos in positions {
            if !assigned
                .iter()
                .any(|(t, p)| t == &pos.topic && *p == pos.partition)
            {
                return Err(EventBrokerError::PartitionNotAssigned {
                    topic: pos.topic.clone(),
                    partition: pos.partition,
                    detail: "seek targets a partition not in the subscription's assignment"
                        .to_owned(),
                    instance: format!("/v1/subscriptions/{id:?}:seek"),
                });
            }
        }
        let mut results: Vec<SeekResult> = Vec::with_capacity(positions.len());
        let mut internal: HashMap<(String, u32), i64> = HashMap::new();
        for pos in positions {
            let offset = match &pos.value {
                ResolvedPosition::Exact(n) => *n,
                ResolvedPosition::Earliest => 0,
                ResolvedPosition::Latest => core
                    .topics
                    .get(&pos.topic)
                    .and_then(|t| t.next_offset.get(&pos.partition).copied())
                    .unwrap_or(0)
                    .saturating_sub(1),
                ResolvedPosition::AtTimestamp(ts) => {
                    resolve_at_timestamp(&core, &pos.topic, pos.partition, ts)
                }
            };
            // A4: the resolved cursor must lie in [RF-1, HWM] = [0, next_offset].
            // (RF=1 on a 1-based log, so RF-1=0; HWM = next offset to be admitted.)
            let hwm = core
                .topics
                .get(&pos.topic)
                .and_then(|t| t.next_offset.get(&pos.partition).copied())
                .unwrap_or(1);
            if offset < 0 || offset > hwm {
                return Err(EventBrokerError::InvalidInitialPosition {
                    topic: pos.topic.clone(),
                    partition: pos.partition,
                    requested: offset.to_string(),
                    detail: format!(
                        "resolved offset {offset} is outside the valid range [0, {hwm}]"
                    ),
                    instance: format!("/v1/subscriptions/{id:?}:seek"),
                });
            }
            results.push(SeekResult {
                topic: pos.topic.clone(),
                partition: pos.partition,
                offset,
            });
            internal.insert((pos.topic.clone(), pos.partition), offset);
        }
        // Advance group cursor using MAX rule (forward-only, equivalent to old ack behaviour).
        let group_id = core.subscriptions.get(&id).map(|s| s.group);
        if let Some(gid) = group_id
            && let Some(group) = core.groups.get_mut(&gid)
        {
            for ((topic, partition), offset) in &internal {
                let entry = group.cursor.entry((topic.clone(), *partition)).or_default();
                entry.offset = entry.offset.max(*offset);
            }
        }
        if let Some(sub) = core.subscriptions.get_mut(&id) {
            sub.expires_at = Instant::now() + sub.session_timeout;
            sub.seek.extend(internal);
        }
        self.notify.notify_waiters();
        Ok(results)
    }

    // -- Introspection ---------------------------------------------------------
    async fn list_topics(&self, _ctx: &SecurityContext) -> Result<Vec<Topic>, EventBrokerError> {
        let core = self.core.lock().await;
        Ok(core
            .topics
            .keys()
            .map(|id| Topic {
                id: id.clone(),
                description: None,
                partitions: core.topics[id].partitions,
                retention: None,
                streaming: None,
                created_at: Utc::now(),
            })
            .collect())
    }

    async fn list_topic_segments(
        &self,
        _ctx: &SecurityContext,
        topic: &str,
        partition: u32,
        _range: PartitionRange,
    ) -> Result<Vec<TopicSegment>, EventBrokerError> {
        let core = self.core.lock().await;
        let t = core
            .topics
            .get(topic)
            .ok_or_else(|| EventBrokerError::TopicNotFound {
                topic: topic.to_owned(),
                detail: String::new(),
                instance: String::new(),
            })?;
        let log = t.log.get(&partition);
        let segments = if let Some(events) = log {
            if events.is_empty() {
                vec![]
            } else {
                let start = events.first().and_then(|e| e.event.sequence).unwrap_or(0);
                let end = events.last().and_then(|e| e.event.sequence).unwrap_or(0);
                let ts = events
                    .first()
                    .and_then(|e| e.event.sequence_time)
                    .unwrap_or_else(Utc::now);
                let te = events
                    .last()
                    .and_then(|e| e.event.sequence_time)
                    .unwrap_or_else(Utc::now);
                vec![TopicSegment {
                    topic: topic.to_owned(),
                    partition,
                    start_sequence: start,
                    end_sequence: end,
                    start_time: ts,
                    end_time: te,
                    segments: vec![],
                }]
            }
        } else {
            vec![]
        };
        Ok(segments)
    }

    async fn list_event_types(
        &self,
        _ctx: &SecurityContext,
    ) -> Result<Vec<EventType>, EventBrokerError> {
        let core = self.core.lock().await;
        Ok(core
            .topics
            .iter()
            .flat_map(|(topic_id, t)| {
                t.event_types.iter().map(move |(type_id, reg)| EventType {
                    id: type_id.clone(),
                    topic: topic_id.clone(),
                    description: None,
                    data_schema: reg.data_schema.clone(),
                    created_at: Utc::now(),
                })
            })
            .collect())
    }

    async fn get_event_type(
        &self,
        _ctx: &SecurityContext,
        id: &str,
    ) -> Result<EventType, EventBrokerError> {
        let core = self.core.lock().await;
        for (topic_id, t) in &core.topics {
            if let Some(reg) = t.event_types.get(id) {
                return Ok(EventType {
                    id: id.to_owned(),
                    topic: topic_id.clone(),
                    description: None,
                    data_schema: reg.data_schema.clone(),
                    created_at: Utc::now(),
                });
            }
        }
        Err(EventBrokerError::EventTypeUnknown {
            type_id: id.to_owned(),
            detail: format!("event type '{id}' not registered in mock"),
            instance: String::new(),
        })
    }
}
