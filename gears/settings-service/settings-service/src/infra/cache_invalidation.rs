//! The replica's cache invalidation consumer.
//!
//! One `stateful` background task per replica: it owns the replica's anonymous
//! consumer group on the cache topic, joins it with one interest (the
//! `cache_invalidate` type under the root tenant, offsets in memory from the
//! latest position), and hands each delivery to the key-wide eviction of the
//! domain. The broker is a soft dependency: a lost broker pauses the task, which
//! drops its group best-effort and, on resume, creates a new one.
//!
//! The group is driven through the broker API rather than the SDK's
//! `ConsumerBuilder`: the builder creates the group inside its runtime and never
//! hands the id back, so the replica could neither delete it on stop nor keep a
//! refused join on the same group (`dispatcher.rs` `ensure_group`).

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use event_broker_sdk::{
    ConsumerGroupId, ControlCode, CreateConsumerGroupRequest, EventBrokerApi, EventBrokerError,
    FrameStream, GtsIdPattern, GtsInstanceId, JoinRequest, Position, SeekPosition, SubscriptionId,
    WireEvent, WireFrame, api::SubscriptionInterest,
};
use futures_util::StreamExt;
use settings_service_sdk::gts::{CACHE_INVALIDATE_EVENT_TYPE, CACHE_TOPIC_ID};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use tracing::{debug, info, info_span, warn};

use crate::domain::coherence::consume_cache_invalidation;
use crate::domain::resolution::EffectiveCache;
use crate::infra::event_broker::{Backoff, EventBrokerClient, or_cancelled};

/// How long stop spends leaving and deleting the group: under the gear
/// lifecycle's `stop_timeout` of 30 s, so a slow broker never costs the stop.
const CLEANUP_DEADLINE: Duration = Duration::from_secs(10);

/// The share of the cleanup deadline `leave` may spend; the group deletion gets
/// what remains, even when `leave` timed out.
const LEAVE_SHARE: u32 = 3;

/// How often a quiet stream is interrupted to look the broker up again; the
/// broker's own heartbeat cadence is of this order.
const LIVENESS_PERIOD: Duration = Duration::from_secs(5);

/// How long a stream stays open before it counts as healthy even with no frame
/// on it: a quiet topic is not a failing broker.
const HEALTHY_STREAM: Duration = Duration::from_secs(5);

/// The session timeout the join asks for.
const SESSION_TIMEOUT: Duration = Duration::from_secs(60);

/// Why the replica stopped holding a group.
enum Ended {
    Cancelled,
    Lost,
}

/// What ended one stream.
enum Taken {
    Ended(Ended),
    /// The broker closed the subscription with a terminal control frame: join
    /// again through the same group.
    Rejoin,
}

/// Why a join did not yield a stream.
enum OpenFailed {
    /// The broker refused a step; the reason is logged and the join retried.
    Refused(String),
    /// The stop fired while a step was in flight.
    Cancelled,
}

/// Await one broker call of the join, cut short by the stop: a call that never
/// answers must not hold the task past the lifecycle's stop.
async fn step<T, E: std::fmt::Display>(
    cancel: &CancellationToken,
    call: &'static str,
    work: impl Future<Output = Result<T, E>>,
) -> Result<T, OpenFailed> {
    match or_cancelled(cancel, work).await {
        None => {
            warn!(call, "stop interrupted an in-flight broker call");
            Err(OpenFailed::Cancelled)
        }
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(OpenFailed::Refused(error.to_string())),
    }
}

/// The stream has proved healthy: the backoff starts over, once per stream.
fn prove(pause: &mut Backoff, proven: &mut bool) {
    if !*proven {
        pause.reset();
        *proven = true;
    }
}

/// The group the replica holds and the subscription it has on it, if any.
struct Held {
    broker: Arc<dyn EventBrokerApi>,
    ctx: SecurityContext,
    group: ConsumerGroupId,
    subscription: Option<SubscriptionId>,
}

/// The per-replica consumer of the `cache_invalidate` broadcast.
// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-consumer:p1
// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-soft-dependency:p1
// @cpt-dod:cpt-cf-settings-service-dod-replica-coherence-observability:p1
pub struct CacheInvalidationConsumer {
    client: Arc<EventBrokerClient>,
    cache: Arc<EffectiveCache>,
    cleanup_deadline: Duration,
    liveness: Duration,
    healthy: Duration,
}

impl CacheInvalidationConsumer {
    /// A consumer evicting from `cache` on what `client`'s broker delivers.
    #[must_use]
    pub const fn new(client: Arc<EventBrokerClient>, cache: Arc<EffectiveCache>) -> Self {
        Self {
            client,
            cache,
            cleanup_deadline: CLEANUP_DEADLINE,
            liveness: LIVENESS_PERIOD,
            healthy: HEALTHY_STREAM,
        }
    }

    /// Replace the stop deadline, the liveness period and the healthy-stream
    /// period; for tests.
    #[must_use]
    pub const fn with_timing(
        mut self,
        cleanup_deadline: Duration,
        liveness: Duration,
        healthy: Duration,
    ) -> Self {
        self.cleanup_deadline = cleanup_deadline;
        self.liveness = liveness;
        self.healthy = healthy;
        self
    }

    /// Run until `cancel` fires, then leave and delete the group best-effort.
    pub async fn run(&self, cancel: CancellationToken) {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-2
        info!("cache invalidation consumer started");
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-2
        let mut pause = self.client.backoff();
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-3
        while !cancel.is_cancelled() && self.cycle(&mut pause, &cancel).await {}
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-3
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-11
        info!("cache invalidation consumer stopped");
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-11
    }

    /// One pass of the lifecycle loop: resolve, hold a group, take deliveries,
    /// release the group. `false` when the token fired and the task is done.
    async fn cycle(&self, pause: &mut Backoff, cancel: &CancellationToken) -> bool {
        if !self.client.resolve_until_ready(cancel).await {
            return false;
        }
        let mut held = match self.acquire(pause, cancel).await {
            Ok(held) => held,
            Err(Ended::Cancelled) => return false,
            Err(Ended::Lost) => return self.lost(None, pause, cancel).await,
        };
        match self.hold(&mut held, pause, cancel).await {
            Ended::Cancelled => {
                // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-9
                self.release(&held, "stop").await;
                // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-9
                false
            }
            Ended::Lost => self.lost(Some(&held), pause, cancel).await,
        }
    }

    /// The broker is lost: pause, make the one best-effort attempt to drop the
    /// group if one is held, and wait before resolving again. `false` when the
    /// token fired meanwhile.
    async fn lost(
        &self,
        held: Option<&Held>,
        pause: &mut Backoff,
        cancel: &CancellationToken,
    ) -> bool {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-8
        warn!("event broker lost; cache invalidation consumer paused");
        if let Some(held) = held {
            self.release(held, "loss").await;
        }
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-8
        EventBrokerClient::wait_cycle(pause, cancel).await
    }

    /// The resolved broker, the publishing identity and the replica's one group.
    async fn acquire(
        &self,
        pause: &mut Backoff,
        cancel: &CancellationToken,
    ) -> Result<Held, Ended> {
        let (Some(broker), Some(ctx)) = (self.client.broker(), self.client.publishing_context())
        else {
            return Err(Ended::Lost);
        };
        let group = self.create_group(&broker, &ctx, pause, cancel).await?;
        Ok(Held {
            broker,
            ctx,
            group,
            subscription: None,
        })
    }

    /// Create the replica's anonymous group once, retrying a refusal with
    /// backoff until it holds or the broker is lost.
    async fn create_group(
        &self,
        broker: &Arc<dyn EventBrokerApi>,
        ctx: &SecurityContext,
        pause: &mut Backoff,
        cancel: &CancellationToken,
    ) -> Result<ConsumerGroupId, Ended> {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-12
        loop {
            let request = CreateConsumerGroupRequest {
                client_agent: self.client.group_alias(),
                description: Some("settings-service replica cache invalidation".to_owned()),
            };
            let Some(answer) =
                or_cancelled(cancel, broker.create_consumer_group(ctx, request)).await
            else {
                warn!(
                    call = "create_consumer_group",
                    "stop interrupted an in-flight broker call"
                );
                return Err(Ended::Cancelled);
            };
            match answer {
                Ok(group) => return Ok(group.id),
                Err(error) => {
                    warn!(%error, "consumer group refused by the broker; retrying");
                    if !EventBrokerClient::wait_cycle(pause, cancel).await {
                        return Err(Ended::Cancelled);
                    }
                    if self.client.broker().is_none() {
                        return Err(Ended::Lost);
                    }
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-12
    }

    /// Join through the held group and take deliveries; a refused join is
    /// retried against the same group.
    async fn hold(
        &self,
        held: &mut Held,
        pause: &mut Backoff,
        cancel: &CancellationToken,
    ) -> Ended {
        loop {
            match self.session(held, pause, cancel).await {
                Ok(Taken::Ended(ended)) => return ended,
                Ok(Taken::Rejoin) => debug!("subscription closed by the broker; joining again"),
                Err(OpenFailed::Cancelled) => return Ended::Cancelled,
                Err(OpenFailed::Refused(reason)) => {
                    warn!(%reason, "cache topic join refused; retrying on the same group");
                }
            }
            if !EventBrokerClient::wait_cycle(pause, cancel).await {
                return Ended::Cancelled;
            }
            if self.client.broker().is_none() {
                return Ended::Lost;
            }
        }
    }

    /// One join and the deliveries taken through it.
    async fn session(
        &self,
        held: &mut Held,
        pause: &mut Backoff,
        cancel: &CancellationToken,
    ) -> Result<Taken, OpenFailed> {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-6
        let stream = self.open(held, cancel).await?;
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-6
        Ok(self.take(stream, pause, cancel).await)
    }

    /// One join: the single interest, the seek to the latest position, the
    /// stream. Any step's failure is the reason a refusal is logged with, and
    /// the stop cuts any step short.
    async fn open(
        &self,
        held: &mut Held,
        cancel: &CancellationToken,
    ) -> Result<FrameStream, OpenFailed> {
        // Best effort: the previous subscription may already be gone.
        if let Some(previous) = held.subscription.take() {
            match step(cancel, "leave", held.broker.leave(&held.ctx, previous)).await {
                Ok(()) => {}
                Err(OpenFailed::Cancelled) => return Err(OpenFailed::Cancelled),
                Err(OpenFailed::Refused(error)) => {
                    debug!(%error, "previous subscription not left");
                }
            }
        }
        let root = self
            .client
            .root_tenant()
            .ok_or_else(|| OpenFailed::Refused("the root tenant is not known".to_owned()))?;
        let refused = |e: &dyn std::fmt::Display| OpenFailed::Refused(e.to_string());
        let topic = GtsInstanceId::try_new(CACHE_TOPIC_ID).map_err(|e| refused(&e))?;
        let pattern =
            GtsIdPattern::try_new(CACHE_INVALIDATE_EVENT_TYPE).map_err(|e| refused(&e))?;
        let interest = SubscriptionInterest::builder()
            .topic(topic)
            .tenant_id(root)
            .types([pattern])
            .build()
            .map_err(|e| refused(&e))?;
        let assignment = step(
            cancel,
            "join",
            held.broker.join(
                &held.ctx,
                JoinRequest {
                    group: held.group,
                    client_agent: self.client.group_alias(),
                    interests: vec![interest],
                    session_timeout: Some(SESSION_TIMEOUT),
                },
            ),
        )
        .await?;
        held.subscription = Some(assignment.subscription_id);
        // Offsets live in memory and start from the latest position: a
        // broadcast published before this join is bounded by the TTL.
        let positions: Vec<SeekPosition> = assignment
            .assigned
            .iter()
            .map(|a| SeekPosition {
                topic: a.topic.clone(),
                partition: a.partition,
                value: Position::Latest,
            })
            .collect();
        step(
            cancel,
            "seek",
            held.broker.seek(
                &held.ctx,
                assignment.subscription_id,
                assignment.topology_version,
                &positions,
            ),
        )
        .await?;
        step(
            cancel,
            "stream",
            held.broker.stream(&held.ctx, assignment.subscription_id),
        )
        .await
    }

    /// Take deliveries until the token fires or the broker is lost, looking the
    /// broker up again before every wait for the next frame. The backoff starts
    /// over once the stream has proved healthy: its first event or
    /// non-terminal frame arrived, or it stayed open for the healthy period.
    async fn take(
        &self,
        mut stream: FrameStream,
        pause: &mut Backoff,
        cancel: &CancellationToken,
    ) -> Taken {
        let opened = Instant::now();
        let mut proven = false;
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-7
        loop {
            if self.client.broker().is_none() {
                return Taken::Ended(Ended::Lost);
            }
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Taken::Ended(Ended::Cancelled),
                () = tokio::time::sleep(self.liveness) => {
                    if opened.elapsed() >= self.healthy {
                        prove(pause, &mut proven);
                    }
                }
                frame = stream.next() => {
                    if let Some(taken) = self.on_next(frame, pause, &mut proven) {
                        return taken;
                    }
                }
            }
        }
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-7
    }

    /// What the stream yielded: its end or an error is the broker lost. A
    /// delivery is taken and, like any other non-terminal frame, proves the
    /// stream healthy; a terminal control frame ends the subscription and
    /// proves nothing.
    fn on_next(
        &self,
        next: Option<Result<WireFrame, EventBrokerError>>,
        pause: &mut Backoff,
        proven: &mut bool,
    ) -> Option<Taken> {
        match next {
            None => {
                warn!("delivery stream ended");
                Some(Taken::Ended(Ended::Lost))
            }
            Some(Err(error)) => {
                warn!(%error, "delivery stream failed");
                Some(Taken::Ended(Ended::Lost))
            }
            Some(Ok(WireFrame::Event(event))) => {
                prove(pause, proven);
                self.deliver(&event);
                None
            }
            Some(Ok(WireFrame::Control {
                code: ControlCode::Terminal,
                ..
            })) => Some(Taken::Rejoin),
            Some(Ok(_)) => {
                prove(pause, proven);
                None
            }
        }
    }

    /// One delivery: the consume flow, inside a `cache_invalidate.consume` span
    /// that continues the publisher's trace when the event carries a
    /// `trace_parent`. The eviction is idempotent and a malformed body only
    /// warns, so every delivery is complete on return.
    fn deliver(&self, event: &WireEvent) {
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-1
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-2
        if event.type_id.as_ref() != CACHE_INVALIDATE_EVENT_TYPE {
            debug!(event_id = %event.id, "event of another type ignored");
            return;
        }
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-2
        let span = info_span!(
            "cache_invalidate.consume",
            change_set_id = tracing::field::Empty,
            tenant = tracing::field::Empty,
        );
        if let Some(traceparent) = event.trace_parent.as_deref()
            && let Ok(value) = http::HeaderValue::from_str(traceparent)
        {
            let mut headers = http::HeaderMap::new();
            headers.insert(toolkit_trace_context::TRACEPARENT, value);
            toolkit_trace_context::set_parent_from_headers(&span, &headers);
        }
        for field in ["change_set_id", "tenant"] {
            if let Some(value) = event.data.get(field).and_then(|v| v.as_str()) {
                span.record(field, value);
            }
        }
        span.in_scope(|| {
            consume_cache_invalidation(&self.cache, event.id, Some(&event.data));
        });
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-consume:p1:inst-rc-consume-1
    }

    /// Leave the subscription and delete the group, once, best-effort, within
    /// the deadline. The replica holds no group afterwards whatever happened: a
    /// failure leaves the group in the broker as a crashed replica's is.
    async fn release(&self, held: &Held, why: &str) {
        let started = Instant::now();
        // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-10
        if let Some(subscription) = held.subscription {
            bounded(
                self.cleanup_deadline / LEAVE_SHARE,
                "leaving the cache topic failed",
                why,
                held.broker.leave(&held.ctx, subscription),
            )
            .await;
        }
        bounded(
            self.cleanup_deadline.saturating_sub(started.elapsed()),
            "consumer group not deleted; it stays in the broker",
            why,
            held.broker.delete_consumer_group(&held.ctx, &held.group),
        )
        .await;
        // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-10
    }
}

/// One cleanup call within `limit`: a refusal and a timeout are each a warning.
async fn bounded<E: std::fmt::Display>(
    limit: Duration,
    failed: &str,
    why: &str,
    call: impl Future<Output = Result<(), E>>,
) {
    match tokio::time::timeout(limit, call).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => warn!(%error, why, "{failed}"),
        Err(_) => warn!(why, "{failed}: passed its share of the cleanup deadline"),
    }
}

#[cfg(test)]
#[path = "cache_invalidation_tests.rs"]
mod cache_invalidation_tests;
