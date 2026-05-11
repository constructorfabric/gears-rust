use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use toolkit_security::SecurityContext;
use tracing::{trace, warn};

use futures_util::StreamExt;

use crate::api::{
    AssignedPartition, EventBroker, JoinRequest, ResolvedPosition, SubscriptionAssignment,
};
use crate::api::{
    BarrierMode, ControlCode, Filter, PartitionPosition, PartitionSlot, SeekPosition,
    SubscriptionInterest, TenantTraversalDepth, WireEvent, WireFrame,
};
use crate::consumer::commit::CommitHandle;
use crate::consumer::offset_manager::CommitOffset;
use crate::consumer::{
    BatchEventHandler, ConsumerCommitMode, ConsumerGroupRef, DeadLetterEvent, EventBatch,
    HandlerOutcome, RawEvent,
};
use crate::error::EventBrokerError;
use crate::ids::{ConsumerGroupId, SubscriptionId, TopicId};

/// Per-partition in-memory cursor (latest acked offset).
#[derive(Default)]
struct PartitionCursor {
    latest_offset: i64,
    committed: i64, // the last offset actually committed to CommitOffset
}

/// Runs one subscription slot: JOIN, poll, dispatch, retry, re-JOIN.
pub(crate) struct SlotDispatcher<H, OM>
where
    H: BatchEventHandler<CommitHandle, HandlerOutcome> + 'static,
    OM: CommitOffset + 'static,
{
    pub slot_idx: u32,
    pub broker: Arc<dyn EventBroker>,
    pub offset_manager: Arc<OM>,
    pub handler: Arc<H>,
    pub group_ref: ConsumerGroupRef,
    pub topics: Vec<String>,
    pub tenant_id: Option<uuid::Uuid>,
    pub tenant_depth: TenantTraversalDepth,
    pub barrier_mode: BarrierMode,
    pub event_type_patterns: Vec<String>,
    pub client_agent: String,
    pub session_timeout: Option<Duration>,
    pub filter: Option<Filter>,
    pub heartbeat_drop_threshold: usize,
    pub retry_base: Duration,
    pub retry_max: Duration,
    pub commit_mode: ConsumerCommitMode,
    pub max_rejoin_attempts: u32,
    pub subscription_id: Arc<tokio::sync::Mutex<Option<SubscriptionId>>>,
    /// Optional DLQ callback. When `Some`, the dispatcher can handle `Reject` outcomes.
    pub on_dead_letter: Option<
        Arc<
            dyn Fn(
                    DeadLetterEvent,
                ) -> std::pin::Pin<
                    Box<dyn std::future::Future<Output = Result<(), EventBrokerError>> + Send>,
                > + Send
                + Sync,
        >,
    >,
}

impl<H, OM> SlotDispatcher<H, OM>
where
    H: BatchEventHandler<CommitHandle, HandlerOutcome> + 'static,
    OM: CommitOffset + 'static,
{
    pub async fn run(
        &self,
        ctx: Arc<SecurityContext>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), EventBrokerError> {
        let group_id = self.ensure_group(&ctx).await?;
        let mut assignment = self.join_subscription(&ctx, &group_id).await?;
        {
            let mut guard = self.subscription_id.lock().await;
            *guard = Some(assignment.subscription_id);
        }

        // Pre-stream SEEK: resolve a starting position per assigned partition
        // via OffsetStore::load_position(...) and seed them on the broker before
        // opening the stream. Without this, the broker returns `409
        // PositionsNotSet` on stream-open (defensive backstop).
        self.resolve_and_seek(
            &ctx,
            &group_id,
            assignment.subscription_id,
            &assignment.assigned,
        )
        .await?;

        let cursors: Arc<RwLock<HashMap<PartitionSlot, PartitionCursor>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Auto-commit timer task.
        let _commit_task = match self.commit_mode {
            ConsumerCommitMode::Auto { interval } => {
                let seek_cursors = cursors.clone();
                let seek_broker = self.broker.clone();
                let seek_om = self.offset_manager.clone();
                let seek_ctx = ctx.clone();
                let seek_sub = assignment.subscription_id;
                let seek_topics = self.topics.clone();
                let seek_cancel = cancel.clone();
                Some(tokio::spawn(async move {
                    let mut interval = tokio::time::interval(interval);
                    loop {
                        tokio::select! {
                            _ = seek_cancel.cancelled() => break,
                            _ = interval.tick() => {
                                let guard = seek_cursors.read().await;
                                for (slot, cursor) in guard.iter() {
                                    // Skip if handler already committed this offset (suppress double-write).
                                    if cursor.latest_offset > cursor.committed {
                                        let topic_string = seek_topics
                                            .get(slot.topic_ix as usize)
                                            .cloned()
                                            .unwrap_or_default();
                                        let topic_id = TopicId::from_gts(&topic_string);
                                        let _ = seek_om.commit(
                                            &ConsumerGroupId::new(uuid::Uuid::nil()),
                                            &topic_id,
                                            slot.partition,
                                            cursor.latest_offset,
                                        ).await;
                                        let _ = seek_broker.seek(
                                            &seek_ctx,
                                            seek_sub,
                                            &[SeekPosition {
                                                topic: topic_string,
                                                partition: slot.partition,
                                                value: ResolvedPosition::Exact(cursor.latest_offset),
                                            }],
                                        ).await;
                                    }
                                }
                            }
                        }
                    }
                }))
            }
            ConsumerCommitMode::Manual => None,
        };

        let mut consecutive_failures = 0u32;
        let mut topology_version = assignment.topology_version;

        'outer: loop {
            if cancel.is_cancelled() {
                break;
            }

            let sub_id = assignment.subscription_id;
            let mut stream = match self.broker.stream(&ctx, sub_id).await {
                Ok(s) => s,
                Err(EventBrokerError::SubscriptionRecoveryExhausted { .. }) => {
                    return Err(EventBrokerError::SubscriptionRecoveryExhausted {
                        attempts: consecutive_failures,
                        detail: "stream open: recovery exhausted".into(),
                        instance: String::new(),
                    });
                }
                Err(EventBrokerError::PositionsNotSet { unseeded, .. }) => {
                    // Defensive recovery: re-resolve via position() and SEEK the
                    // unseeded partitions, then retry. Shares the
                    // SubscriptionRecoveryExhausted budget.
                    consecutive_failures += 1;
                    if consecutive_failures > self.max_rejoin_attempts {
                        return Err(EventBrokerError::SubscriptionRecoveryExhausted {
                            attempts: consecutive_failures,
                            detail: "stream open: PositionsNotSet recovery exhausted".into(),
                            instance: String::new(),
                        });
                    }
                    let slots = self.slots_for_unseeded(&unseeded);
                    self.resolve_and_seek(&ctx, &group_id, sub_id, &slots)
                        .await?;
                    continue;
                }
                Err(e) => {
                    warn!(error = %e, "stream open failed; re-joining");
                    assignment = self
                        .rejoin(&ctx, &group_id, &mut consecutive_failures)
                        .await?;
                    self.resolve_and_seek(
                        &ctx,
                        &group_id,
                        assignment.subscription_id,
                        &assignment.assigned,
                    )
                    .await?;
                    continue;
                }
            };

            let mut consecutive_heartbeats: usize = 0;
            consecutive_failures = 0;

            while let Some(frame_result) = stream.next().await {
                if cancel.is_cancelled() {
                    break 'outer;
                }
                match frame_result {
                    Ok(WireFrame::Event(event)) => {
                        consecutive_heartbeats = 0;
                        self.dispatch_event(&ctx, event, &cursors, sub_id, topology_version)
                            .await;
                    }
                    Ok(WireFrame::Heartbeat { .. }) => {
                        consecutive_heartbeats += 1;
                        if consecutive_heartbeats >= self.heartbeat_drop_threshold {
                            trace!(
                                threshold = self.heartbeat_drop_threshold,
                                "drop-on-Nth-heartbeat triggered; voluntary disconnect + re-JOIN"
                            );
                            // Drop the stream by breaking out of the inner loop.
                            // Outer loop will rejoin.
                            break;
                        }
                    }
                    Ok(WireFrame::Control {
                        code,
                        positions,
                        reason,
                    }) => {
                        // Cursor carrier: feed positions into the offset store so a
                        // reconnect re-SEEKs from last_examined, skipping
                        // server-side-filtered events (R57).
                        self.commit_positions(&ctx, &group_id, &positions).await;
                        if code == ControlCode::Terminal {
                            // Gain / lose-all: the subscription is terminating. Final
                            // positions are committed above; re-JOIN (outer loop).
                            trace!(?reason, "terminal control frame; re-JOIN");
                            break;
                        }
                        // Progress: sparse mid-stream update; keep streaming.
                    }
                    Ok(WireFrame::Topology {
                        topology_version: tv,
                        assigned,
                    }) => {
                        topology_version = tv;
                        // Non-terminal: a partition loss or a `topology_version`-only
                        // change. Commit the snapshot's positions and update the
                        // assignment (lost partitions drop off); keep streaming.
                        // Gains never arrive as a topology frame - they terminate via
                        // a Terminal control frame and a re-JOIN.
                        self.commit_positions(&ctx, &group_id, &assigned).await;
                    }
                    Err(EventBrokerError::Transport(_)) => {
                        break; // outer loop rejoins
                    }
                    Err(e) => {
                        warn!(error = %e, "stream frame error; ending stream");
                        break;
                    }
                }
            }

            // Stream ended (subscription gone, connection closed, or drop-on-Nth-heartbeat).
            // Re-JOIN unless we're cancelled.
            if cancel.is_cancelled() {
                break;
            }
            assignment = self
                .rejoin(&ctx, &group_id, &mut consecutive_failures)
                .await?;
            topology_version = assignment.topology_version;
            // Fresh subscription_id → no SEEK history on the broker → must
            // re-seed positions before opening the next stream.
            self.resolve_and_seek(
                &ctx,
                &group_id,
                assignment.subscription_id,
                &assignment.assigned,
            )
            .await?;
        }

        // Graceful shutdown: leave subscription.
        let _ = self.broker.leave(&ctx, assignment.subscription_id).await;
        Ok(())
    }

    async fn dispatch_event(
        &self,
        _ctx: &SecurityContext,
        wire: WireEvent,
        cursors: &Arc<RwLock<HashMap<PartitionSlot, PartitionCursor>>>,
        _sub_id: SubscriptionId,
        _topology_version: i64,
    ) {
        let slot = PartitionSlot {
            topic_ix: 0,
            partition: wire.partition,
        };

        let raw = RawEvent {
            id: wire.id,
            type_id: wire.type_id.clone(),
            topic: wire.topic.clone(),
            tenant_id: wire.tenant_id,
            subject: wire.subject,
            subject_type: wire.subject_type,
            partition: wire.partition,
            sequence: wire.sequence,
            offset: wire.offset,
            occurred_at: wire.occurred_at,
            sequence_time: wire.sequence_time,
            trace_parent: wire.trace_parent,
            data: wire.data,
        };

        let mut attempts: u16 = 1;
        let mut backoff = self.retry_base;

        loop {
            let commit_handle = CommitHandle::new(wire.partition, wire.offset);
            let committed_flag = commit_handle.committed.clone();
            let event = [raw.clone()];
            let mut batch = EventBatch::new(&event);
            match self
                .handler
                .handle_batch(&mut batch, attempts, commit_handle)
                .await
            {
                Ok(HandlerOutcome::Success) => {
                    // Advance in-memory cursor.
                    let mut guard = cursors.write().await;
                    let c = guard.entry(slot).or_default();
                    let already_committed =
                        committed_flag.load(std::sync::atomic::Ordering::Acquire);
                    c.latest_offset = c.latest_offset.max(wire.offset);
                    // If handler already committed via commit_in_tx or commit_on_eb,
                    // mark as committed so the auto-commit timer skips this offset.
                    if already_committed {
                        c.committed = wire.offset;
                    }
                    break;
                }
                Ok(HandlerOutcome::Retry { reason }) => {
                    trace!(reason, attempts, "handler returned Retry; backing off");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(self.retry_max);
                    attempts = attempts.saturating_add(1);
                }
                Err(e) => {
                    warn!(error = %e, "handler returned Err; treating as Retry");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(self.retry_max);
                    attempts = attempts.saturating_add(1);
                }
            }
        }
    }

    /// Route a `Reject` outcome: advance cursor + fire DLQ callback (retried with backoff
    /// until callback succeeds so dead-letters always land). Used by the WithDlq variant.
    async fn route_reject(
        &self,
        ctx: &SecurityContext,
        raw: &RawEvent,
        reason: String,
        attempts: u16,
        cursors: &Arc<RwLock<HashMap<PartitionSlot, PartitionCursor>>>,
        sub_id: SubscriptionId,
    ) {
        let slot = PartitionSlot {
            topic_ix: 0,
            partition: raw.partition,
        };

        // Advance cursor past the rejected event regardless of ack mode (spec D10 / 19.9).
        {
            let mut guard = cursors.write().await;
            let c = guard.entry(slot).or_default();
            c.latest_offset = c.latest_offset.max(raw.offset);
            c.committed = raw.offset; // mark committed so auto-commit sees it
        }
        // Best-effort broker seek for the cursor advance.
        let _ = self
            .broker
            .seek(
                ctx,
                sub_id,
                &[SeekPosition {
                    topic: raw.topic.clone(),
                    partition: raw.partition,
                    value: ResolvedPosition::Exact(raw.offset),
                }],
            )
            .await;

        // Fire DLQ callback. Retry with backoff until it succeeds (dead-letter MUST land).
        if let Some(ref cb) = self.on_dead_letter {
            let dl = DeadLetterEvent {
                event: raw.clone(),
                reason,
                attempts,
            };
            let mut backoff = self.retry_base;
            loop {
                match cb(dl.clone()).await {
                    Ok(()) => break,
                    Err(e) => {
                        warn!(error = %e, "on_dead_letter callback failed; retrying");
                        tokio::time::sleep(backoff).await;
                        backoff = (backoff * 2).min(self.retry_max);
                    }
                }
            }
        }
    }

    /// Resolve a starting position for each slot via `CommitOffset::position`
    /// and SEEK the broker. Used after JOIN (initial seed), after re-JOIN, on
    /// Topology-frame for newly-assigned partitions, and on `409
    /// PositionsNotSet` recovery.
    async fn resolve_and_seek(
        &self,
        ctx: &SecurityContext,
        group_id: &ConsumerGroupId,
        subscription_id: SubscriptionId,
        slots: &[AssignedPartition],
    ) -> Result<(), EventBrokerError> {
        if slots.is_empty() {
            return Ok(());
        }
        let mut positions: Vec<SeekPosition> = Vec::with_capacity(slots.len());
        for slot in slots {
            let topic = slot.topic.clone();
            let topic_id = TopicId::from_gts(&topic);
            let value = self
                .offset_manager
                .load_position(group_id, &topic_id, slot.partition)
                .await?;
            positions.push(SeekPosition {
                topic,
                partition: slot.partition,
                value,
            });
        }
        self.broker
            .seek(ctx, subscription_id, &positions)
            .await
            .map(|_| ())
    }

    /// Feed control / topology-frame positions into the consumer's own offset
    /// store. Persists `last_examined` (the scan frontier) so a later reconnect
    /// re-SEEKs past server-side-filtered events (R57). Best-effort: a failure to
    /// persist is logged, not fatal.
    async fn commit_positions(
        &self,
        _ctx: &SecurityContext,
        group_id: &ConsumerGroupId,
        positions: &[PartitionPosition],
    ) {
        for p in positions {
            let topic = match self.topic_for_slot(&p.slot) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let topic_id = TopicId::from_gts(&topic);
            if let Err(e) = self
                .offset_manager
                .commit(group_id, &topic_id, p.slot.partition, p.last_examined)
                .await
            {
                warn!(error = %e, "failed to commit control-frame position to offset store");
            }
        }
    }

    /// Look up a partition's topic name from the slot's `topic_ix`, indexed
    /// into the dispatcher's declared topics. The broker assigns indices in
    /// the order the SDK declared interests.
    fn topic_for_slot(&self, slot: &PartitionSlot) -> Result<String, EventBrokerError> {
        self.topics
            .get(slot.topic_ix as usize)
            .cloned()
            .ok_or_else(|| {
                EventBrokerError::Internal(format!(
                    "topology returned topic_ix={} but only {} topics were declared",
                    slot.topic_ix,
                    self.topics.len()
                ))
            })
    }

    /// Translate `(topic, partition)` pairs reported by `409 PositionsNotSet`
    /// into the public assignment shape used by `EventBroker::join`.
    fn slots_for_unseeded(&self, unseeded: &[(String, u32)]) -> Vec<AssignedPartition> {
        unseeded
            .iter()
            .map(|(topic, partition)| AssignedPartition {
                topic: topic.clone(),
                partition: *partition,
            })
            .collect()
    }

    async fn ensure_group(
        &self,
        ctx: &SecurityContext,
    ) -> Result<ConsumerGroupId, EventBrokerError> {
        match &self.group_ref {
            ConsumerGroupRef::Id(id) => Ok(id.clone()),
            ConsumerGroupRef::Gts(gts) => Err(EventBrokerError::InvalidConsumerOptions {
                detail: format!(
                    "consumer group GTS reference '{gts}' must be resolved before startup"
                ),
                instance: String::new(),
            }),
            ConsumerGroupRef::AutoAnonymous { alias } => {
                let group = self
                    .broker
                    .create_consumer_group(
                        ctx,
                        crate::models::CreateConsumerGroupRequest {
                            client_agent: alias.clone(),
                            description: None,
                        },
                    )
                    .await?;
                Ok(group.id)
            }
        }
    }

    async fn join_subscription(
        &self,
        ctx: &SecurityContext,
        group_id: &ConsumerGroupId,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        let tenant_id = self.tenant_id.unwrap_or_else(uuid::Uuid::nil);
        let event_type_patterns = if self.event_type_patterns.is_empty() {
            vec!["*".to_owned()]
        } else {
            self.event_type_patterns.clone()
        };
        let interests: Vec<SubscriptionInterest> = self
            .topics
            .iter()
            .map(|t| {
                let mut builder = SubscriptionInterest::builder()
                    .topic(t.clone())
                    .tenant_id(tenant_id)
                    .tenant_depth(self.tenant_depth)
                    .barrier_mode(self.barrier_mode)
                    .types(event_type_patterns.clone());
                if let Some(filter) = self.filter.clone() {
                    builder = builder.filter(filter);
                }
                builder.build()
            })
            .collect::<Result<_, _>>()?;

        self.broker
            .join(
                ctx,
                JoinRequest {
                    group: group_id.clone(),
                    client_agent: self.client_agent.clone(),
                    interests,
                    session_timeout: self.session_timeout,
                },
            )
            .await
    }

    async fn rejoin(
        &self,
        ctx: &SecurityContext,
        group_id: &ConsumerGroupId,
        consecutive_failures: &mut u32,
    ) -> Result<SubscriptionAssignment, EventBrokerError> {
        *consecutive_failures += 1;
        if *consecutive_failures > self.max_rejoin_attempts {
            return Err(EventBrokerError::SubscriptionRecoveryExhausted {
                attempts: *consecutive_failures,
                detail: "max re-JOIN attempts exceeded".into(),
                instance: String::new(),
            });
        }
        tokio::time::sleep(Duration::from_millis(250) * (*consecutive_failures)).await;
        let assignment = self.join_subscription(ctx, group_id).await?;
        {
            let mut guard = self.subscription_id.lock().await;
            *guard = Some(assignment.subscription_id);
        }
        Ok(assignment)
    }
}
