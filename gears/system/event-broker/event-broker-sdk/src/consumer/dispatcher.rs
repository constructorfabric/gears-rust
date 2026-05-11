use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use toolkit_security::SecurityContext;
use tokio::sync::RwLock;
use tracing::{trace, warn};

use futures_util::StreamExt;

use crate::consumer::ack::AckHandle;
use crate::consumer::backend::{
    BarrierMode, ConsumerBackend, Filter, PartitionSlot, SeekPosition, SubscriptionAssignment,
    SubscriptionInterest,
    WireEvent, WireFrame,
};
use crate::consumer::offset_manager::ResolvedPosition;
use crate::consumer::offset_manager::OffsetManager;
use crate::consumer::{ConsumerGroupRef, DeadLetterEvent, EventHandler, HandlerOutcome, RawEvent};
use crate::error::EventBrokerError;
use crate::ids::{ConsumerGroupId, SubscriptionId};

/// Per-partition in-memory cursor (latest acked offset).
#[derive(Default)]
struct PartitionCursor {
    latest_offset: i64,
    committed: i64, // the last offset actually committed to OffsetManager
}

/// Runs one subscription slot: JOIN, poll, dispatch, retry, re-JOIN.
pub(crate) struct SlotDispatcher<H, OM>
where
    H: EventHandler<AckHandle, HandlerOutcome> + 'static,
    OM: OffsetManager + 'static,
{
    pub slot_idx: u32,
    pub backend: Arc<dyn ConsumerBackend>,
    pub offset_manager: Arc<OM>,
    pub handler: Arc<H>,
    pub group_ref: ConsumerGroupRef,
    pub topics: Vec<String>,
    pub tenant_id: Option<uuid::Uuid>,
    pub max_depth: Option<u32>,
    pub barrier_mode: BarrierMode,
    pub event_type_patterns: Vec<String>,
    pub client_agent: String,
    pub session_timeout: Option<String>,
    pub filter: Option<Filter>,
    pub heartbeat_drop_threshold: usize,
    pub retry_base: Duration,
    pub retry_max: Duration,
    pub auto_ack_interval: Option<Duration>,
    pub manual_ack: bool,
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
    H: EventHandler<AckHandle, HandlerOutcome> + 'static,
    OM: OffsetManager + 'static,
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
        // via OffsetManager::position(...) and seed them on the broker before
        // opening the stream. Without this, the broker returns `409
        // PositionsNotSet` on stream-open (defensive backstop).
        let mut assigned_set: HashMap<PartitionSlot, ()> = HashMap::new();
        for slot in &assignment.assigned {
            assigned_set.insert(*slot, ());
        }
        self.resolve_and_seek(
            &ctx,
            &group_id,
            assignment.subscription_id,
            &assignment.assigned,
        )
        .await?;

        let cursors: Arc<RwLock<HashMap<PartitionSlot, PartitionCursor>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // Auto-ack timer task.
        let auto_ack_interval = self.auto_ack_interval.unwrap_or(Duration::from_secs(20));
        let seek_cursors = cursors.clone();
        let seek_backend = self.backend.clone();
        let seek_om = self.offset_manager.clone();
        let seek_ctx = ctx.clone();
        let seek_sub = assignment.subscription_id;
        let seek_topics = self.topics.clone();
        let seek_cancel = cancel.clone();
        let manual_ack = self.manual_ack;
        let _ack_task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(auto_ack_interval);
            loop {
                tokio::select! {
                    _ = seek_cancel.cancelled() => break,
                    _ = interval.tick() => {
                        if manual_ack { continue; }
                        let guard = seek_cursors.read().await;
                        for (slot, cursor) in guard.iter() {
                            // Skip if handler already committed this offset (suppress double-write).
                            if cursor.latest_offset > cursor.committed {
                                let _ = seek_om.save_on_eb(
                                    &seek_ctx,
                                    &ConsumerGroupId(String::new()),
                                    &slot.partition.to_string(),
                                    slot.partition,
                                    cursor.latest_offset,
                                ).await;
                                let topic_string = seek_topics
                                    .get(slot.topic_ix as usize)
                                    .cloned()
                                    .unwrap_or_default();
                                let _ = seek_backend.seek(
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
        });

        let mut consecutive_failures = 0u32;
        let mut topology_version = assignment.topology_version;

        'outer: loop {
            if cancel.is_cancelled() {
                break;
            }

            let sub_id = assignment.subscription_id;
            let mut stream = match self.backend.stream(&ctx, sub_id).await {
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
                    assigned_set.clear();
                    for slot in &assignment.assigned {
                        assigned_set.insert(*slot, ());
                    }
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
                    Ok(WireFrame::Heartbeat) => {
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
                    Ok(WireFrame::Advisory { code, detail }) => {
                        warn!(%code, %detail, "broker advisory");
                    }
                    Ok(WireFrame::Topology {
                        topology_version: tv,
                        assigned,
                        ..
                    }) => {
                        topology_version = tv;
                        // Diff prev vs new — SEEK only partitions newly assigned
                        // to this subscription. Continuing partitions keep
                        // their cursor; lost partitions handled via the
                        // existing 409 PartitionNotAssigned ack path.
                        let mut new_slots: Vec<PartitionSlot> = Vec::new();
                        for slot in &assigned {
                            if !assigned_set.contains_key(slot) {
                                new_slots.push(*slot);
                            }
                        }
                        if !new_slots.is_empty() {
                            if let Err(e) = self
                                .resolve_and_seek(&ctx, &group_id, sub_id, &new_slots)
                                .await
                            {
                                warn!(error = %e, "topology-frame SEEK failed for new partitions");
                                // Surface by closing the stream; outer loop
                                // re-JOINs and re-SEEKs everything.
                                break;
                            }
                        }
                        assigned_set.clear();
                        for slot in &assigned {
                            assigned_set.insert(*slot, ());
                        }
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
            assigned_set.clear();
            for slot in &assignment.assigned {
                assigned_set.insert(*slot, ());
            }
        }

        // Graceful shutdown: leave subscription.
        let _ = self
            .backend
            .delete_subscription(&ctx, assignment.subscription_id)
            .await;
        Ok(())
    }

    async fn dispatch_event(
        &self,
        ctx: &SecurityContext,
        wire: WireEvent,
        cursors: &Arc<RwLock<HashMap<PartitionSlot, PartitionCursor>>>,
        sub_id: SubscriptionId,
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
            let ack = AckHandle::new(wire.partition, wire.offset);
            let committed_flag = ack.committed.clone();
            match self.handler.handle(ctx, raw.clone(), attempts, ack).await {
                Ok(HandlerOutcome::Success) => {
                    // Advance in-memory cursor.
                    let mut guard = cursors.write().await;
                    let c = guard.entry(slot).or_default();
                    let already_committed =
                        committed_flag.load(std::sync::atomic::Ordering::Acquire);
                    c.latest_offset = c.latest_offset.max(wire.offset);
                    // If handler already committed via commit_in_tx or commit_on_eb,
                    // mark as committed so the auto-ack timer skips this offset.
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
            c.committed = raw.offset; // mark committed so auto-ack sees it
        }
        // Best-effort broker seek for the cursor advance.
        let _ = self
            .backend
            .seek(ctx, sub_id, &[SeekPosition {
                topic: raw.topic.clone(),
                partition: raw.partition,
                value: ResolvedPosition::Exact(raw.offset),
            }])
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

    /// Resolve a starting position for each slot via `OffsetManager::position`
    /// and SEEK the broker. Used after JOIN (initial seed), after re-JOIN, on
    /// Topology-frame for newly-assigned partitions, and on `409
    /// PositionsNotSet` recovery.
    async fn resolve_and_seek(
        &self,
        ctx: &SecurityContext,
        group_id: &ConsumerGroupId,
        subscription_id: SubscriptionId,
        slots: &[PartitionSlot],
    ) -> Result<(), EventBrokerError> {
        if slots.is_empty() {
            return Ok(());
        }
        let mut positions: Vec<SeekPosition> = Vec::with_capacity(slots.len());
        for slot in slots {
            let topic = self.topic_for_slot(slot)?;
            let value = self
                .offset_manager
                .position(ctx, group_id, &topic, slot.partition)
                .await?;
            positions.push(SeekPosition {
                topic,
                partition: slot.partition,
                value,
            });
        }
        self.backend.seek(ctx, subscription_id, &positions).await
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

    /// Translate a list of `(topic, partition)` pairs reported by `409
    /// PositionsNotSet` back into `PartitionSlot`s by matching topic names
    /// against the dispatcher's declared topics.
    fn slots_for_unseeded(&self, unseeded: &[(String, u32)]) -> Vec<PartitionSlot> {
        let mut out = Vec::with_capacity(unseeded.len());
        for (topic, partition) in unseeded {
            if let Some(ix) = self.topics.iter().position(|t| t == topic) {
                out.push(PartitionSlot {
                    topic_ix: ix as u16,
                    partition: *partition,
                });
            }
        }
        out
    }

    async fn ensure_group(
        &self,
        ctx: &SecurityContext,
    ) -> Result<ConsumerGroupId, EventBrokerError> {
        match &self.group_ref {
            ConsumerGroupRef::Existing(id) => Ok(id.clone()),
            ConsumerGroupRef::AutoAnonymous {
                client_agent,
                description,
            } => {
                let group = self
                    .backend
                    .create_consumer_group(
                        ctx,
                        crate::models::CreateConsumerGroupRequest {
                            client_agent: client_agent.clone(),
                            description: description.clone(),
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
        let interests: Vec<SubscriptionInterest> = self
            .topics
            .iter()
            .map(|t| SubscriptionInterest {
                topic: t.clone(),
                tenant_id,
                max_depth: self.max_depth,
                barrier_mode: self.barrier_mode,
                types: self.event_type_patterns.clone(),
                filter: self.filter.clone(),
            })
            .collect();

        self.backend
            .create_subscription(
                ctx,
                group_id,
                &self.client_agent,
                self.session_timeout.as_deref(),
                &interests,
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
