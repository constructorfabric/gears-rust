//! The broker consumer-group session both of the gear's consumers run: the
//! replica's cache invalidation consumer and the activation client's group of
//! one subscriber.
//!
//! One anonymous group per cycle, driven through the broker API rather than
//! the SDK's `ConsumerBuilder` — the builder creates the group inside its
//! runtime and never hands the id back, so the consumer could neither delete it
//! on stop nor keep a refused join on the same group. A cycle resolves what it
//! needs, creates the group (a refusal retried with backoff), joins it with the
//! consumer's interests (offsets in memory from the latest position), takes
//! deliveries until the stop, the loss of the broker or the end of the
//! subscription, and leaves and deletes the group best-effort within the
//! cleanup deadline. A refused join is retried on the same group; a stream the
//! broker closes is joined again through it.
//!
//! What differs between the consumers is a [`GroupSpec`]: the interests a join
//! carries and whether they can grow, the identity the calls are made under,
//! what a delivery does, the labels of the group and of the warnings, and the
//! waits.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use event_broker_sdk::{
    ConsumerGroupId, ControlCode, CreateConsumerGroupRequest, EventBrokerApi, EventBrokerError,
    EventV1, FrameStream, JoinRequest, Position, SeekPosition, SubscriptionId, WireFrame,
    api::SubscriptionInterest,
};
use futures_util::{FutureExt as _, StreamExt as _};
use tokio_util::sync::CancellationToken;
use toolkit_security::SecurityContext;
use tracing::{debug, warn};

use super::event_broker::{Backoff, EventBrokerClient, or_cancelled};

/// How long stop spends leaving and deleting the group: under the gear
/// lifecycle's `stop_timeout` of 30 s, so a slow broker never costs the stop.
pub const CLEANUP_DEADLINE: Duration = Duration::from_secs(10);

/// The share of the cleanup deadline `leave` may spend; the group deletion gets
/// what remains, even when `leave` timed out.
pub const LEAVE_SHARE: u32 = 3;

/// How often a quiet stream is interrupted to look the broker up again; the
/// broker's own heartbeat cadence is of this order.
pub const LIVENESS_PERIOD: Duration = Duration::from_secs(5);

/// How long a stream stays open before it counts as healthy even with no frame
/// on it: a quiet topic is not a failing broker.
pub const HEALTHY_STREAM: Duration = Duration::from_secs(5);

/// The session timeout the join asks for.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(60);

/// The waits of one consumer's session.
#[derive(Debug, Clone, Copy)]
pub struct GroupTiming {
    /// How long leaving and deleting the group may take in all.
    pub cleanup_deadline: Duration,
    /// How often a quiet stream is interrupted to look the broker up again.
    pub liveness: Duration,
    /// How long a stream stays open before it counts as healthy.
    pub healthy: Duration,
}

/// What one consumer's group session is made of; the session itself — the
/// cycle, the group's creation, the join, the stream, the release — is this
/// module's and the same for every consumer.
#[allow(
    clippy::redundant_pub_crate,
    reason = "crate-visible by name so a consumer's associated types may stay crate-private (E0446)"
)]
pub(crate) trait GroupSpec: Send + Sync {
    /// What a join's interests were read at, for [`Self::changed`] to compare
    /// against: `()` for a fixed set.
    type Mark: Send;

    /// What the deliveries taken off the stream at once accumulate, completed
    /// together by [`Self::complete`].
    type Taking: Default + Send;

    /// The most frames taken off the stream at once: what already waits behind
    /// a frame, up to this many in all, is taken with it. `1` takes each frame
    /// alone.
    const DRAIN_LIMIT: usize;

    /// The warning of a lost broker.
    const LOST: &'static str;

    /// Whether a loss while no group is held — no broker when a cycle starts,
    /// or none after a refused creation — is warned as [`Self::LOST`] too, or
    /// waited out quietly as the plain absence of a soft dependency.
    const WARN_UNHELD_LOSS: bool;

    /// The warning of a leave that failed on release.
    const LEAVE_FAILED: &'static str;

    /// The broker, when the hub holds one now. A hub lookup, not a call.
    fn broker(&self) -> Option<Arc<dyn EventBrokerApi>>;

    /// The identity a cycle starts under; `None` is a lost broker.
    fn identity(&self) -> Option<SecurityContext>;

    /// The identity of one broker call of a cycle started under `cycle`: by
    /// default that one; a consumer whose identity can change while it holds a
    /// group answers the current one.
    fn call_ctx(&self, cycle: &SecurityContext) -> SecurityContext {
        cycle.clone()
    }

    /// The group's `client_agent`, sent with its creation and every join.
    fn alias(&self) -> String;

    /// The group's description.
    fn description(&self) -> String;

    /// The waits.
    fn timing(&self) -> GroupTiming;

    /// A fresh backoff for the session.
    fn backoff(&self) -> Backoff;

    /// What a cycle waits for before it looks the broker up; `false` when
    /// `cancel` fired. Nothing by default.
    async fn prepare(&self, _cancel: &CancellationToken) -> bool {
        true
    }

    /// The broker refused the group's creation; it is retried after a backoff.
    fn create_refused(&self, reason: &str, grant_missing: bool);

    /// The broker refused a join; it is retried on the same group after a
    /// backoff.
    fn join_refused(&self, reason: &str, grant_missing: bool);

    /// A join succeeded and its stream is open.
    fn joined(&self) {}

    /// The interests of one join and the mark they were read at; an error is
    /// the reason the join is refused with.
    ///
    /// # Errors
    /// The reason an interest cannot be built.
    fn interests(&self) -> Result<(Vec<SubscriptionInterest>, Self::Mark), String>;

    /// Resolves once the interests changed after `mark`; never for a fixed set.
    async fn changed(mark: &mut Self::Mark);

    /// One delivery, taken into `taking`. `false` when `cancel` cut it short.
    async fn on_event(
        &self,
        event: &EventV1,
        taking: &mut Self::Taking,
        cancel: &CancellationToken,
    ) -> bool;

    /// The deliveries taken off the stream at once are all received: complete
    /// them, before a frame among them that ends the stream is acted on.
    fn complete(&self, taking: Self::Taking);
}

/// Why the consumer stopped holding a group.
pub enum Ended {
    /// The stop fired.
    Cancelled,
    /// The broker was lost: no stream, group or subscription is usable.
    Lost,
}

/// What ended one stream.
pub enum Taken {
    /// The stream ended for good: the consumer stops holding the group.
    Ended(Ended),
    /// The broker closed the subscription with a terminal control frame: join
    /// again through the same group, after a backoff.
    Rejoin,
    /// The interests changed: join again at once with the full set.
    Reinterest,
}

/// Why a join did not yield a stream.
pub enum OpenFailed {
    /// The broker refused a step; `grant_missing` says it was for a grant.
    Refused {
        /// The broker's log-safe reason.
        reason: String,
        /// Whether the refusal was for a missing grant.
        grant_missing: bool,
    },
    /// The stop fired while a step was in flight.
    Cancelled,
}

/// Whether the broker's refusal is a missing grant (`define` or `consume`).
fn grant_missing(error: &EventBrokerError) -> bool {
    matches!(error, EventBrokerError::Unauthorized { .. })
}

/// Await one broker call, cut short by the stop: a call that never answers
/// must not hold the task past the lifecycle's stop.
async fn step<T>(
    cancel: &CancellationToken,
    call: &'static str,
    work: impl Future<Output = Result<T, EventBrokerError>>,
) -> Result<T, OpenFailed> {
    match or_cancelled(cancel, work).await {
        None => {
            warn!(call, "stop interrupted an in-flight broker call");
            Err(OpenFailed::Cancelled)
        }
        Some(Ok(value)) => Ok(value),
        Some(Err(error)) => Err(OpenFailed::Refused {
            grant_missing: grant_missing(&error),
            reason: error.to_string(),
        }),
    }
}

/// The stream has proved healthy: the backoff starts over, once per stream.
pub fn prove(pause: &mut Backoff, proven: &mut bool) {
    if !*proven {
        pause.reset();
        *proven = true;
    }
}

/// One cleanup call within `limit`: a refusal and a timeout are each a warning.
pub async fn bounded<E: std::fmt::Display>(
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

type Next = Option<Result<WireFrame, EventBrokerError>>;

/// Whether a frame lets the stream go on: an event or a non-terminal frame.
fn continues(next: &Next) -> bool {
    !matches!(
        next,
        None | Some(
            Err(_)
                | Ok(WireFrame::Control {
                    code: ControlCode::Terminal,
                    ..
                })
        )
    )
}

/// The group the consumer holds, the identity its cycle started under and the
/// subscription it has on the group, if any.
pub struct Held {
    /// The broker the group lives on.
    pub broker: Arc<dyn EventBrokerApi>,
    /// The identity the cycle's calls run under.
    pub ctx: SecurityContext,
    /// The group the consumer holds.
    pub group: ConsumerGroupId,
    /// The subscription on the group; kept until its leave settles.
    pub subscription: Option<SubscriptionId>,
}

/// Run cycles until `cancel` fires; the last one leaves and deletes its group
/// best-effort.
pub async fn run<S: GroupSpec>(spec: &S, cancel: &CancellationToken) {
    let mut pause = spec.backoff();
    while !cancel.is_cancelled() && cycle(spec, &mut pause, cancel).await {}
}

/// One pass: prepare, hold a group on the broker, release the group. `false`
/// when the token fired and the task is done.
async fn cycle<S: GroupSpec>(spec: &S, pause: &mut Backoff, cancel: &CancellationToken) -> bool {
    if !spec.prepare(cancel).await {
        return false;
    }
    let (Some(broker), Some(ctx)) = (spec.broker(), spec.identity()) else {
        // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-25
        // @cpt-begin:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-5
        // No broker resolvable: the activation group's registered keys stay
        // `registered` and the service's delivery loop catches up from the
        // `awaiting` states once the group joins.
        return lost(spec, None, pause, cancel).await;
        // @cpt-end:cpt-cf-settings-service-algo-consumer-activation-subscribe-state:p1:inst-ca-substate-5
        // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-subscribe:p1:inst-ca-subscribe-25
    };
    let group = match create_group(spec, &broker, &ctx, pause, cancel).await {
        Ok(group) => group,
        Err(Ended::Cancelled) => return false,
        Err(Ended::Lost) => return lost(spec, None, pause, cancel).await,
    };
    let mut held = Held {
        broker,
        ctx,
        group,
        subscription: None,
    };
    match hold(spec, &mut held, pause, cancel).await {
        Ended::Cancelled => {
            // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-9
            release(spec, &held, "stop").await;
            // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-9
            false
        }
        Ended::Lost => lost(spec, Some(&held), pause, cancel).await,
    }
}

/// The broker is lost: pause, make the one best-effort attempt to drop the
/// group if one is held, and wait before the next cycle. `false` when the
/// token fired meanwhile.
async fn lost<S: GroupSpec>(
    spec: &S,
    held: Option<&Held>,
    pause: &mut Backoff,
    cancel: &CancellationToken,
) -> bool {
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-8
    if held.is_some() || S::WARN_UNHELD_LOSS {
        warn!("{}", S::LOST);
    }
    if let Some(held) = held {
        release(spec, held, "loss").await;
    }
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-8
    EventBrokerClient::wait_cycle(pause, cancel).await
}

/// Create the consumer's anonymous group once, retrying a refusal with
/// backoff until it holds or the broker is lost.
async fn create_group<S: GroupSpec>(
    spec: &S,
    broker: &Arc<dyn EventBrokerApi>,
    ctx: &SecurityContext,
    pause: &mut Backoff,
    cancel: &CancellationToken,
) -> Result<ConsumerGroupId, Ended> {
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-12
    loop {
        let request = CreateConsumerGroupRequest {
            client_agent: spec.alias(),
            description: Some(spec.description()),
        };
        match step(
            cancel,
            "create_consumer_group",
            broker.create_consumer_group(&spec.call_ctx(ctx), request),
        )
        .await
        {
            Ok(group) => return Ok(group.id),
            Err(OpenFailed::Cancelled) => return Err(Ended::Cancelled),
            Err(OpenFailed::Refused {
                reason,
                grant_missing,
            }) => {
                spec.create_refused(&reason, grant_missing);
                if !EventBrokerClient::wait_cycle(pause, cancel).await {
                    return Err(Ended::Cancelled);
                }
                if spec.broker().is_none() {
                    return Err(Ended::Lost);
                }
            }
        }
    }
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-12
}

/// Join through the held group and take deliveries; a refused join is retried
/// against the same group.
async fn hold<S: GroupSpec>(
    spec: &S,
    held: &mut Held,
    pause: &mut Backoff,
    cancel: &CancellationToken,
) -> Ended {
    loop {
        match session(spec, held, pause, cancel).await {
            Ok(Taken::Ended(ended)) => return ended,
            Ok(Taken::Reinterest) => {
                debug!("a tenant interest was added; joining again");
                continue;
            }
            Ok(Taken::Rejoin) => debug!("subscription closed by the broker; joining again"),
            Err(OpenFailed::Cancelled) => return Ended::Cancelled,
            Err(OpenFailed::Refused {
                reason,
                grant_missing,
            }) => spec.join_refused(&reason, grant_missing),
        }
        if !EventBrokerClient::wait_cycle(pause, cancel).await {
            return Ended::Cancelled;
        }
        if spec.broker().is_none() {
            return Ended::Lost;
        }
    }
}

/// One join and the deliveries taken through it.
async fn session<S: GroupSpec>(
    spec: &S,
    held: &mut Held,
    pause: &mut Backoff,
    cancel: &CancellationToken,
) -> Result<Taken, OpenFailed> {
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-6
    let (stream, mut mark) = open(spec, held, cancel).await?;
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-6
    spec.joined();
    Ok(take(spec, stream, &mut mark, pause, cancel).await)
}

/// One join: the consumer's interests, the seek to the latest position, the
/// stream. Any step's failure is the reason a refusal is logged with, and the
/// stop cuts any step short.
async fn open<S: GroupSpec>(
    spec: &S,
    held: &mut Held,
    cancel: &CancellationToken,
) -> Result<(FrameStream, S::Mark), OpenFailed> {
    // Best effort: the previous subscription may already be gone. It stays on
    // `held` until the leave settles, so a stop that cancels the leave still
    // finds it and `release` leaves it before the group is deleted.
    if let Some(previous) = held.subscription {
        match step(
            cancel,
            "leave",
            held.broker.leave(&spec.call_ctx(&held.ctx), previous),
        )
        .await
        {
            Ok(()) => held.subscription = None,
            Err(OpenFailed::Cancelled) => return Err(OpenFailed::Cancelled),
            Err(OpenFailed::Refused { reason, .. }) => {
                debug!(%reason, "previous subscription not left");
                held.subscription = None;
            }
        }
    }
    // The set the join carries and the mark it was read at, together: a change
    // after this point wakes the stream.
    let (interests, mark) = spec.interests().map_err(|reason| OpenFailed::Refused {
        reason,
        grant_missing: false,
    })?;
    let assignment = step(
        cancel,
        "join",
        held.broker.join(
            &spec.call_ctx(&held.ctx),
            JoinRequest {
                group: held.group,
                client_agent: spec.alias(),
                interests,
                session_timeout: Some(SESSION_TIMEOUT),
            },
        ),
    )
    .await?;
    held.subscription = Some(assignment.subscription_id);
    // Offsets live in memory and start from the latest position: what was
    // published before this join is not replayed — a cache broadcast is
    // bounded by the cache's TTL, a change notification is the service's
    // delivery loop's to catch up.
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
            &spec.call_ctx(&held.ctx),
            assignment.subscription_id,
            assignment.topology_version,
            &positions,
        ),
    )
    .await?;
    let stream = step(
        cancel,
        "stream",
        held.broker
            .stream(&spec.call_ctx(&held.ctx), assignment.subscription_id),
    )
    .await?;
    Ok((stream, mark))
}

/// Take deliveries until the token fires, the broker is lost or the interests
/// change, looking the broker up again before every wait for the next frame.
/// The backoff starts over once the stream has proved healthy: its first event
/// or non-terminal frame arrived, or it stayed open for the healthy period.
pub async fn take<S: GroupSpec>(
    spec: &S,
    mut stream: FrameStream,
    mark: &mut S::Mark,
    pause: &mut Backoff,
    cancel: &CancellationToken,
) -> Taken {
    let timing = spec.timing();
    let opened = Instant::now();
    let mut proven = false;
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-7
    loop {
        if spec.broker().is_none() {
            return Taken::Ended(Ended::Lost);
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Taken::Ended(Ended::Cancelled),
            () = S::changed(mark) => return Taken::Reinterest,
            () = tokio::time::sleep(timing.liveness) => {
                if opened.elapsed() >= timing.healthy {
                    prove(pause, &mut proven);
                }
            }
            frame = stream.next() => {
                // Whatever already waits behind it is taken with it.
                let mut frames = vec![frame];
                while frames.len() < S::DRAIN_LIMIT && frames.last().is_some_and(continues) {
                    match stream.next().now_or_never() {
                        Some(next) => frames.push(next),
                        None => break,
                    }
                }
                if let Some(taken) = on_frames(spec, frames, pause, &mut proven, cancel).await {
                    return taken;
                }
            }
        }
    }
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-7
}

/// The frames taken off the stream at once, in order: the deliveries among
/// them are received one by one and completed together, once, after the last
/// of them — before a frame that ends the stream is acted on.
async fn on_frames<S: GroupSpec>(
    spec: &S,
    frames: Vec<Next>,
    pause: &mut Backoff,
    proven: &mut bool,
    cancel: &CancellationToken,
) -> Option<Taken> {
    let mut taking = S::Taking::default();
    let mut ended = None;
    for next in frames {
        ended = on_next(spec, next, pause, proven, &mut taking, cancel).await;
        if ended.is_some() {
            break;
        }
    }
    spec.complete(taking);
    ended
}

/// What the stream yielded: its end or an error is the broker lost. A delivery
/// is received into `taking` and, like any other non-terminal frame, proves
/// the stream healthy; a terminal control frame ends the subscription and
/// proves nothing.
async fn on_next<S: GroupSpec>(
    spec: &S,
    next: Next,
    pause: &mut Backoff,
    proven: &mut bool,
    taking: &mut S::Taking,
    cancel: &CancellationToken,
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
            let received = spec.on_event(&event, taking, cancel).await;
            (!received).then_some(Taken::Ended(Ended::Cancelled))
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

/// Leave the subscription and delete the group, once, best-effort, within the
/// cleanup deadline. The consumer holds no group afterwards whatever happened:
/// a failure leaves the group in the broker, as a crashed process's is.
pub async fn release<S: GroupSpec>(spec: &S, held: &Held, why: &str) {
    let deadline = spec.timing().cleanup_deadline;
    let started = Instant::now();
    // @cpt-begin:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-10
    // @cpt-begin:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-13
    if let Some(subscription) = held.subscription {
        bounded(
            deadline / LEAVE_SHARE,
            S::LEAVE_FAILED,
            why,
            held.broker.leave(&spec.call_ctx(&held.ctx), subscription),
        )
        .await;
    }
    bounded(
        deadline.saturating_sub(started.elapsed()),
        "consumer group not deleted; it stays in the broker",
        why,
        held.broker
            .delete_consumer_group(&spec.call_ctx(&held.ctx), &held.group),
    )
    .await;
    // @cpt-end:cpt-cf-settings-service-flow-consumer-activation-unsubscribe:p1:inst-ca-unsubscribe-13
    // @cpt-end:cpt-cf-settings-service-flow-replica-coherence-lifecycle:p1:inst-rc-lifecycle-10
}

#[cfg(test)]
#[path = "broker_group_lifecycle_tests.rs"]
pub mod lifecycle_tests;
