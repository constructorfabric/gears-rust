use std::collections::HashMap;
use std::time::Instant;

use async_trait::async_trait;
use chrono::Utc;
use toolkit_security::SecurityContext;
use uuid::Uuid;

use crate::ResolvedPosition;
use crate::api::{AssignedPartition, EventBroker, JoinRequest, SeekResult, SubscriptionAssignment};
use crate::consumer::backend::{FrameStream, SeekPosition, SubscriptionInterest};
use crate::error::EventBrokerError;
use crate::ids::{ConsumerGroupId, ProducerId, SubscriptionId};
use crate::internal::envelope::Event as WireEnvelope;
use crate::models::{
    ConsumerGroup, ConsumerGroupKind, CreateConsumerGroupRequest, EventType, Page, PartitionRange,
    Subscription, Topic, TopicSegment,
};
use crate::producer::backend::{IngestOutcome, ProducerCursor};

use super::core::{Core, GroupReg, GroupState, MockBroker, SubState};
use super::ingest::{ingest_batch, ingest_one};
use super::rebalance::run_rebalance;
use super::stream::open_stream;

// ─── Helper ────────────────────────────────────────────────────────────────────

fn principal(ctx: &SecurityContext) -> String {
    ctx.subject_id().to_string()
}

fn tenant(ctx: &SecurityContext) -> Uuid {
    ctx.subject_tenant_id()
}

#[async_trait]
impl EventBroker for MockBroker {
    // ── Producer ──────────────────────────────────────────────────────────────
    async fn register_producer(
        &self,
        ctx: &SecurityContext,
        mode_str: &str,
        client_agent: &str,
    ) -> Result<ProducerId, EventBrokerError> {
        let id = ProducerId(Uuid::new_v4());
        let mut core = self.core.lock().await;
        core.producers.insert(
            id,
            super::core::ProducerReg {
                mode_str: mode_str.to_owned(),
                owner_principal: principal(ctx),
                owner_tenant: tenant(ctx),
            },
        );
        let _ = client_agent; // stored for logs in production; no-op in mock
        Ok(id)
    }

    async fn publish(
        &self,
        _ctx: &SecurityContext,
        event: &WireEnvelope,
    ) -> Result<IngestOutcome, EventBrokerError> {
        // Check reject_persist fault.
        {
            let faults = self.faults.lock().await;
            if let Some(reason) = &faults.reject_persist {
                return Err(EventBrokerError::Internal(reason.clone()));
            }
        }
        let mut core = self.core.lock().await;
        let (outcome, _) = ingest_one(&mut core, event)?;
        if outcome == IngestOutcome::Accepted {
            self.notify.notify_waiters();
        }
        Ok(outcome)
    }

    async fn publish_batch(
        &self,
        _ctx: &SecurityContext,
        events: &[WireEnvelope],
    ) -> Result<Vec<IngestOutcome>, EventBrokerError> {
        {
            let faults = self.faults.lock().await;
            if let Some(reason) = &faults.reject_persist {
                return Err(EventBrokerError::Internal(reason.clone()));
            }
        }
        let mut core = self.core.lock().await;
        let results = ingest_batch(&mut core, events)?;
        let any_accepted = results.iter().any(|(o, _)| *o == IngestOutcome::Accepted);
        if any_accepted {
            self.notify.notify_waiters();
        }
        Ok(results.into_iter().map(|(o, _)| o).collect())
    }

    async fn producer_cursors(
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
        topic: Option<&str>,
        partition: Option<u32>,
    ) -> Result<(), EventBrokerError> {
        let mut core = self.core.lock().await;
        match (topic, partition) {
            (Some(t), Some(p)) => {
                // Reset single (producer, topic, partition).
                core.producer_state.remove(&(producer_id, t.to_owned(), p));
            }
            (Some(t), None) => {
                // Reset all (producer, topic, *) — M7 branch 2.
                let keys: Vec<_> = core
                    .producer_state
                    .keys()
                    .filter(|(pid, tp, _)| *pid == producer_id && tp == t)
                    .cloned()
                    .collect();
                for k in keys {
                    core.producer_state.remove(&k);
                }
            }
            _ => {
                // Reset all (producer, *, *) — M7 branch 1 (reset-all).
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

    // ── Consumer groups ───────────────────────────────────────────────────────
    async fn create_consumer_group(
        &self,
        ctx: &SecurityContext,
        _req: CreateConsumerGroupRequest,
    ) -> Result<ConsumerGroup, EventBrokerError> {
        let uuid = Uuid::new_v4();
        let gts_id = format!("gts.cf.core.events.consumer_group.v1~{uuid}");
        let group_id = ConsumerGroupId(gts_id.clone());
        let mut core = self.core.lock().await;
        core.groups_registry.insert(
            group_id.clone(),
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
                group_id: id.clone(),
                detail: format!("consumer group '{}' not found", id.0),
                instance: String::new(),
            }
        })?;
        Ok(ConsumerGroup {
            id: id.clone(),
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
        limit: Option<u32>,
        _cursor: Option<String>,
        _filter: Option<String>,
        _orderby: Option<String>,
    ) -> Result<Page<ConsumerGroup>, EventBrokerError> {
        // Mock: cursor, filter, and orderby are accepted but not implemented.
        // Returns the first page only.
        let page_limit = limit.unwrap_or(25) as usize;
        let core = self.core.lock().await;
        let items: Vec<ConsumerGroup> = core
            .groups_registry
            .iter()
            .take(page_limit)
            .map(|(id, reg)| ConsumerGroup {
                id: id.clone(),
                tenant_id: reg.owner_tenant,
                owner_principal_id: reg.owner_principal.clone(),
                kind: reg.kind,
                description: None,
                created_at: Utc::now(),
            })
            .collect();
        let total = core.groups_registry.len();
        let next_cursor = if total > page_limit { Some("mock-next-cursor".to_owned()) } else { None };
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
                group_id: id.clone(),
                detail: format!("consumer group '{}' not found", id.0),
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
                    detail: format!("consumer group '{}' has active subscriptions", id.0),
                    instance: String::new(),
                });
            }
        }
        core.groups_registry.remove(id);
        core.groups.remove(id);
        Ok(())
    }

    // ── Subscriptions ─────────────────────────────────────────────────────────
    async fn join(
        &self,
        ctx: &SecurityContext,
        req: JoinRequest,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let now = Instant::now();
        let sub_id = SubscriptionId(Uuid::new_v4());
        let timeout = req
            .session_timeout
            .unwrap_or(std::time::Duration::from_secs(30));

        let mut core = self.core.lock().await;

        // Validate group exists.
        if !core.groups_registry.contains_key(&req.group) {
            return Err(EventBrokerError::ConsumerGroupNotFound {
                group_id: req.group.clone(),
                detail: format!("consumer group '{}' not registered", req.group.0),
                instance: String::new(),
            });
        }

        // Build the set of topics from interests.
        let topics: std::collections::HashSet<String> =
            req.interests.iter().map(|i| i.topic.clone()).collect();

        // Build SubState.
        let sub = SubState {
            group: req.group.clone(),
            client_agent: req.client_agent,
            interests: req.interests,
            topics,
            assigned: Vec::new(),
            topology_version: 0,
            created_at: now,
            session_timeout: timeout,
            last_seen_at: now,
            expires_at: now + timeout,
            seek: HashMap::new(),
            sent: HashMap::new(),
        };
        core.subscriptions.insert(sub_id, sub);

        // Ensure GroupState exists.
        if !core.groups.contains_key(&req.group) {
            core.groups.insert(req.group.clone(), GroupState::new());
        }
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
            expires_at: Utc::now() + chrono::Duration::from_std(timeout).unwrap_or_default(),
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
            consumer_group: sub.group.clone(),
            assigned: sub
                .assigned
                .iter()
                .map(|(_, p)| crate::models::PartitionAssignment {
                    topic_ix: 0,
                    partition: *p,
                })
                .collect(),
            topology_version,
            expires_at: Utc::now()
                + chrono::Duration::from_std(sub.session_timeout).unwrap_or_default(),
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
                    consumer_group: sub.group.clone(),
                    assigned: sub
                        .assigned
                        .iter()
                        .map(|(_, p)| crate::models::PartitionAssignment {
                            topic_ix: 0,
                            partition: *p,
                        })
                        .collect(),
                    topology_version: tv,
                    expires_at: Utc::now()
                        + chrono::Duration::from_std(sub.session_timeout).unwrap_or_default(),
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
        if let Some(sub) = core.subscriptions.remove(&id) {
            let group_id = sub.group.clone();
            if let Some(group) = core.groups.get_mut(&group_id) {
                group.members.retain(|m| *m != id);
            }
            run_rebalance(&group_id, &mut core);
            self.notify.notify_waiters();
        }
        Ok(())
    }

    async fn stream(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
    ) -> Result<FrameStream, EventBrokerError> {
        Ok(open_stream(self.clone(), id))
    }

    async fn seek(
        &self,
        _ctx: &SecurityContext,
        id: SubscriptionId,
        positions: &[SeekPosition],
    ) -> Result<Vec<SeekResult>, EventBrokerError> {
        let mut core = self.core.lock().await;
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
                ResolvedPosition::AtTimestamp(_) => {
                    // Mock: timestamp resolution is not implemented; resolve to Earliest (offset 0).
                    0
                }
            };
            results.push(SeekResult {
                topic: pos.topic.clone(),
                partition: pos.partition,
                offset,
            });
            internal.insert((pos.topic.clone(), pos.partition), offset);
        }
        // Advance group cursor using MAX rule (forward-only, equivalent to old ack behaviour).
        let group_id = core
            .subscriptions
            .get(&id)
            .map(|s| s.group.clone());
        if let Some(gid) = group_id {
            if let Some(group) = core.groups.get_mut(&gid) {
                for ((topic, partition), offset) in &internal {
                    let entry = group.cursor.entry((topic.clone(), *partition)).or_default();
                    entry.offset = entry.offset.max(*offset);
                }
            }
        }
        if let Some(sub) = core.subscriptions.get_mut(&id) {
            sub.seek.extend(internal);
        }
        self.notify.notify_waiters();
        Ok(results)
    }

    // ── Introspection ─────────────────────────────────────────────────────────
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
