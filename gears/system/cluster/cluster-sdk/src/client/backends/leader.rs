// Created: 2026-08-13 by Constructor Tech
//! The remote leader-election backend — DESIGN-DEPLOYABLE-GEAR §12.12.
//!
//! Three unary methods that are one RPC each, and one — `elect` — that is a
//! whole state machine, because [`LeaderWatch`] is a *managed* election and the
//! wire carries only the lease.
//!
//! # Transitions are derived client-side, in both deployment profiles
//!
//! The server announces no leadership. It cannot: a holder keeps its claim by
//! renewing, and renewal is client-driven precisely so that renewal stays the
//! consumer-liveness proxy (§7.3, invariant I8). So the pump derives its own
//! status exactly the way the in-process default's renewal loop does:
//!
//! | Transition | Derived from |
//! |---|---|
//! | leader keeps the claim | `renew` answering `Ok` |
//! | leader loses the claim | `renew` answering `lock_expired` — lapsed, stolen, or fenced out, deliberately indistinguishable (§5.8.1) |
//! | follower wins the claim | a re-`join` on the renewal cadence coming back `Leader` |
//!
//! The `await_change` subscription carries only what a client *cannot* derive:
//! `Closed(Shutdown)` and the `Status(Lost)` that precedes it on a drain, plus
//! `Lagged` and `Reset` (§6.6). That symmetry is what keeps invariant I1 true
//! here — one consumer source file, one set of observable transitions.
//!
//! # A follower's re-`join` mints a server-side subscription every time
//!
//! `join` calls `ElectionSubscriptions::open` unconditionally
//! (`cluster/src/api/grpc/leader.rs`), and this pump keeps its **original**
//! `election_id` and stream across a re-join, so every re-claim attempt leaves an
//! *unattached* subscription behind. Nothing closes it today: the serving gear
//! never calls `close`, and the sweep that would is item `S2`, gated on `A6`.
//!
//! Unattached-and-abandoned is precisely the class `A6` specifies a sweep for, so
//! this needs no new mechanism — but it turns that sweep from hygiene into a
//! **prerequisite for Profile 3**, because a steady-state follower produces one
//! per renewal interval (one every 10 s on the default config) rather than one per
//! crashed client. Recorded in the plan's Appendix A, and **shipped as `S2`**: the
//! server sweeps abandoned subscriptions on a 5 s cadence (§5.4.1). The cheaper
//! server-side alternative — `join` reusing the caller's existing subscription for
//! the same election — turned out to be unavailable, because under v1's
//! `TrustedNetwork` mode every caller resolves to one name and `(caller, election)`
//! therefore identifies the fleet rather than a participant.
//!
//! # Dropping the watch stops the pump, and that is load-bearing
//!
//! [`LeaderWatch`]'s contract is that `Drop` does no I/O and leadership lapses
//! through TTL expiry. Lapsing requires that **something stop renewing**, and the
//! only thing renewing is this pump — so the resign channel closing is not merely
//! a tidy-up signal, it is what makes the documented contract true.
//!
//! It was missed once, in a way worth recording because the code read as correct.
//! The arm was written `Some(responder) = resigns.recv()`, and a `select!` branch
//! whose refutable pattern fails is **disabled for that iteration** rather than
//! taken — so the `None` a dropped watch produces was silently discarded on every
//! pass and a leader whose consumer had vanished renewed its claim forever. In
//! Profile 1 the same arm is a `match` with an explicit `None => break`, so the two
//! profiles disagreed about whether a dropped watch ever frees the election, which
//! invariant I1 forbids. Found while testing `S2`, and asserted in both profiles
//! now rather than in neither.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt as _;
use tonic::Streaming;

use super::{RemoteProfile, provider};
use crate::client::backends::cache::duration_ms;
use crate::client::remote::{LeaderStub, decode};
use crate::convert::{LeaseContext, from_lease_status, from_status, to_leader_watch_event};
use crate::descriptors::DescriptorCache;
use crate::dto;
use crate::error::ClusterError;
use crate::grpc::stubs::leader as stubs;
use crate::leader::{
    ElectionConfig, LeaderElectionBackend, LeaderElectionFeatures, LeaderStatus, LeaderWatch,
    LeaderWatchEvent, LeaderWatchSender, ResignReceiver,
};
use crate::lease::LeaseToken;

/// How many events one election watch buffers.
///
/// The same size the server's subscription buffer uses, and for the same reason:
/// an election emits transitions, not a stream of mutations, so a subscriber 32
/// events behind is not slow — it is gone.
const EVENT_BUFFER: usize = 32;

/// The re-attach schedule after a *subscription-level* close (§6.6, §5.4.1).
///
/// Bounded, and bounded by arithmetic rather than taste. The serving gear sweeps
/// abandoned subscriptions on a **5 s** cadence with a **3×** grace window
/// (§5.4.1) and refreshes `last_seen` on every pass that observes a live reader,
/// so when a stream breaks the entry is at most one cadence stale and survives at
/// least `15 s − 5 s = 10 s` more. These delays put the last attempt at 6.3 s,
/// inside that guarantee with room for a round trip, and stop well before the
/// point where a re-`attach` could only ever answer `NotFound`.
///
/// An unbounded schedule would be a new bug, not a safer one: past the grace
/// window the subscription is provably gone, and retrying forever would be one
/// doomed RPC per election per interval for the life of the process.
const REATTACH_BACKOFF: [Duration; 6] = [
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
    Duration::from_millis(1600),
    Duration::from_millis(3200),
];

/// Decodes a status raised **against the subscription itself** (`SEAM-5`).
///
/// `AwaitChange` addresses a subscription, which is replica-local, so the server
/// returns a bare `NotFound` whenever the replica serving it goes away — and
/// §6.9's table maps that row to `Closed(ClusterError::Shutdown)`: terminal and
/// non-retryable, so `RestartingWatch` propagates rather than resubscribing and
/// the consumer's recovery is an explicit re-`elect`. Decoding with
/// [`LeaseContext::None`] instead yields `Provider{Other}` carrying
/// "unrecognised cluster error", which happens to agree on retryability and
/// disagrees on everything a consumer reads.
///
/// Total: [`LeaseContext::ElectionSubscription`] never decodes to release-by-
/// absence, so the fallback is unreachable and is written out rather than
/// unwrapped.
fn from_subscription_status(status: &tonic::Status) -> ClusterError {
    from_lease_status(status, LeaseContext::ElectionSubscription).unwrap_or(ClusterError::Shutdown)
}

/// [`LeaderElectionBackend`] over the wire (§12.12).
#[derive(Debug, Clone)]
pub struct RemoteLeaderElectionBackend {
    stub: LeaderStub,
    profile: RemoteProfile,
}

impl RemoteLeaderElectionBackend {
    /// Binds a handle to `profile` over `stub`.
    pub fn new(stub: LeaderStub, profile: &str, descriptors: Arc<DescriptorCache>) -> Self {
        Self {
            stub,
            profile: RemoteProfile::new(profile, descriptors),
        }
    }

    /// The cached leader-election descriptor, if one has been fetched.
    fn describe(&self) -> Option<dto::LeaderElectionDescriptor> {
        self.profile
            .descriptor()
            .map(|profile| profile.leader_election)
    }

    fn stub(&self) -> LeaderStub {
        self.stub.clone()
    }

    /// The lease reference `renew` and `resign` carry.
    fn lease_ref(&self, token: &LeaseToken, ttl: Option<Duration>) -> stubs::LeaseRef {
        stubs::LeaseRef::from(dto::LeaseRef {
            profile: self.profile.name(),
            token: dto::LeaseToken::from(token.clone()),
            ttl_ms: ttl.map(duration_ms),
            client_request_id: None,
        })
    }

    /// One `Join` RPC, decoded.
    async fn join_once(
        &self,
        name: &str,
        config: ElectionConfig,
    ) -> Result<dto::LeaderJoined, ClusterError> {
        let request = stubs::JoinRequest::from(dto::JoinRequest {
            profile: self.profile.name(),
            name: name.to_owned(),
            ttl_ms: duration_ms(config.ttl()),
            max_missed_renewals: Some(u64::from(config.max_missed_renewals())),
            client_request_id: None,
        });
        let response = self
            .stub()
            .join(request)
            .await
            .map_err(|status| from_status(&status))?;
        decode::<dto::LeaderJoined, _>(response.into_inner())
    }

    /// Opens the election's event subscription.
    async fn subscribe(
        &self,
        election_id: &str,
    ) -> Result<Streaming<stubs::LeaderWatchEventDto>, ClusterError> {
        let request = stubs::AwaitChangeRequest::from(dto::AwaitChangeRequest {
            profile: self.profile.name(),
            election_id: election_id.to_owned(),
        });
        Ok(self
            .stub()
            .await_change(request)
            .await
            .map_err(|status| from_subscription_status(&status))?
            .into_inner())
    }

    /// `join` plus the pump that keeps the claim and reports its transitions.
    async fn enrol(&self, name: &str, config: ElectionConfig) -> Result<LeaderWatch, ClusterError> {
        let joined = self.join_once(name, config).await?;
        // **Read `initial_status`, never the token's shape.** A follower receives
        // the zero token (empty name and owner, `fence: 0`) because
        // `LeaderJoined.token` is not optional on the wire, and branching on that
        // instead would make a follower believe it holds a lease (§6.6).
        let initial = LeaderStatus::from(joined.initial_status);
        let token = matches!(initial, LeaderStatus::Leader).then(|| LeaseToken::from(joined.token));
        let stream = match self.subscribe(&joined.election_id).await {
            Ok(stream) => stream,
            Err(error) => {
                // `join_once` already **took the lease server-side**, and no pump
                // exists yet — so nothing renews it and nothing gives it back.
                // Returning here without resigning holds the election name for a
                // full TTL (30 s on the default config) on behalf of a call that
                // is about to report failure. Give it back on the way out: the
                // same best-effort resign `release_if_holder` makes, and the same
                // obligation `run` already accepts when the consumer has vanished
                // before the first status could be delivered.
                //
                // Guarded by `token`, which is `Some` only for a **winner** and is
                // derived from `initial_status` rather than from the token's
                // shape: a follower receives the zero token (empty name and
                // owner, `fence: 0`) because `LeaderJoined.token` is not optional
                // on the wire, and resigning that would send a meaningless
                // predicate to the server on every lost election (§6.6).
                if let Some(token) = &token {
                    let _best_effort = self.resign(token).await;
                }
                return Err(error);
            }
        };

        let (sender, resigns, watch) = LeaderWatch::channel(EVENT_BUFFER, initial);
        let pump = ElectionPump {
            backend: self.clone(),
            name: name.to_owned(),
            election_id: joined.election_id,
            config,
            token,
            am_leader: matches!(initial, LeaderStatus::Leader),
            missed: 0,
            sender,
        };
        tokio::spawn(pump.run(initial, stream, resigns));
        Ok(watch)
    }
}

#[async_trait]
impl LeaderElectionBackend for RemoteLeaderElectionBackend {
    /// From the descriptor cache (§5.5), on the same fail-safe terms as the
    /// cache's and lock's.
    fn features(&self) -> LeaderElectionFeatures {
        self.describe().map_or_else(
            || LeaderElectionFeatures::new(false),
            |leader| leader.features.into(),
        )
    }

    /// The **server-side** provider (§5.5).
    fn provider_name(&self) -> &'static str {
        provider(self.describe().map(|leader| leader.provider))
    }

    async fn elect(&self, name: &str) -> Result<LeaderWatch, ClusterError> {
        self.enrol(name, ElectionConfig::default()).await
    }

    async fn elect_with_config(
        &self,
        name: &str,
        config: ElectionConfig,
    ) -> Result<LeaderWatch, ClusterError> {
        self.enrol(name, config).await
    }

    /// `owner` is advisory — the server mints the real one from the transport
    /// caller, for the same reason the lock's `acquire` does.
    ///
    /// `None` is losing the election, which is an ordinary outcome rather than an
    /// error. It is read off `initial_status`, never off the token.
    async fn join(
        &self,
        name: &str,
        owner: &str,
        config: ElectionConfig,
    ) -> Result<Option<LeaseToken>, ClusterError> {
        let _server_mints_the_owner = owner;
        let joined = self.join_once(name, config).await?;
        Ok(matches!(
            LeaderStatus::from(joined.initial_status),
            LeaderStatus::Leader
        )
        .then(|| LeaseToken::from(joined.token)))
    }

    /// # Errors
    /// [`ClusterError::LockExpired`] when the token matches no live claim. The
    /// caller turns that into `Status(Lost)` and keeps its subscription open —
    /// losing a claim is a status change, never a terminal close (§6.6).
    async fn renew(&self, token: &LeaseToken, ttl: Duration) -> Result<(), ClusterError> {
        let request = self.lease_ref(token, Some(ttl));
        match self.stub().renew(request).await {
            Ok(_ack) => Ok(()),
            Err(status) => Err(from_lease_status(
                &status,
                LeaseContext::ElectionRenew { name: &token.name },
            )
            .unwrap_or_else(|| ClusterError::LockExpired {
                name: token.name.clone(),
            })),
        }
    }

    /// **Absence is `Ok`**, exactly as the lock's `release` is: a claim that
    /// already lapsed, or was fenced out by a successor, resigns successfully and
    /// leaves the successor's claim untouched (§6.10).
    async fn resign(&self, token: &LeaseToken) -> Result<(), ClusterError> {
        let request = self.lease_ref(token, None);
        match self.stub().resign(request).await {
            Ok(_ack) => Ok(()),
            Err(status) => match from_lease_status(&status, LeaseContext::LeaseRelease) {
                Some(error) => Err(error),
                None => Ok(()),
            },
        }
    }

    /// Left at the trait's `Ok(())` default, for the same reason as the other two
    /// handles': this backend owns no resource the serving gear's readiness check
    /// is asking about.
    async fn probe(&self) -> Result<(), ClusterError> {
        Ok(())
    }
}

/// The per-election task: renew, re-claim, forward, resign (§12.12).
struct ElectionPump {
    backend: RemoteLeaderElectionBackend,
    name: String,
    /// Addresses the *subscription*, and stays valid across a broken stream — so
    /// a re-`attach` inside the server's grace window needs no fresh `join`
    /// (§6.6, §5.4.1). Held here because that affordance is what makes losing the
    /// feed cost a re-subscribe rather than the claim.
    election_id: String,
    config: ElectionConfig,
    /// The claim's authority while this participant holds it; `None` as a
    /// follower.
    token: Option<LeaseToken>,
    am_leader: bool,
    /// Consecutive **transient** renewal failures. A transport error is no
    /// evidence about the lease, so it is counted rather than acted on, and the
    /// budget is the same one the in-process backend uses.
    missed: u8,
    sender: LeaderWatchSender,
}

impl ElectionPump {
    /// Runs until the consumer resigns, drops the watch, or the subscription
    /// closes.
    async fn run(
        mut self,
        initial: LeaderStatus,
        stream: Streaming<stubs::LeaderWatchEventDto>,
        mut resigns: ResignReceiver,
    ) {
        // The resolved initial status, as an event. `LeaderWatch::channel` seeded
        // the snapshot with it; this is what an event-stream consumer sees.
        if self.sender.send_status(initial).await.is_err() {
            // The consumer is already gone. Give the claim back rather than
            // holding it to its deadline for nobody.
            self.release_if_holder().await;
            return;
        }

        let interval = self.config.renewal_interval();
        // A recomputed **absolute** deadline rather than a fresh relative `sleep`
        // built inside the `select!` on every pass, for the reason Profile 1
        // already uses one (`cluster/src/defaults/leader.rs`): a timer rebuilt
        // each iteration restarts the renewal countdown whenever *another* arm
        // fires. With the re-attach schedule below on a 100 ms backoff and a
        // 300 ms cadence, that would push the first renewal past the TTL and cost
        // the claim — the exact failure this whole change exists to remove.
        let mut next_tick = tokio::time::Instant::now() + interval;
        // The subscription, and the re-attach owed when it breaks. `None` is a
        // pump with no feed: it keeps renewing off the timer alone, which is what
        // Profile 1's `None => cache_watch = None` does, and what ADR-003's
        // "Watch task and renewal task: independent signal paths" requires.
        let mut stream = Some(stream);
        let mut reattach: Option<Reattach> = None;
        loop {
            let tick = tokio::time::sleep_until(next_tick);
            tokio::pin!(tick);
            tokio::select! {
                // Renewal is what *holds* leadership, so it is a timer and not a
                // reaction to anything the server says (§7.3).
                () = &mut tick => {
                    if !self.tick().await {
                        break;
                    }
                    next_tick = tokio::time::Instant::now() + interval;
                }
                frame = next_frame(&mut stream) => {
                    match self.forward(frame).await {
                        Pumped::Continue => {}
                        // The *subscription* ended; the claim is untouched.
                        Pumped::Resubscribe => {
                            stream = None;
                            reattach = Some(Reattach::start());
                        }
                        Pumped::Stop => break,
                    }
                }
                // Owed only while the feed is down, so this arm is inert in the
                // steady state and can never displace the renewal above.
                () = due(reattach.as_ref().map(Reattach::at)) => {
                    if !self.reattach(&mut stream, &mut reattach, interval).await {
                        break;
                    }
                }
                // Deliberately binds the whole `Option`, rather than the
                // `Some(responder) = ..` this used to be. A `select!` branch whose
                // refutable pattern fails is **disabled** for that iteration
                // instead of taken, so the `None` the channel yields when the
                // consumer drops its watch was silently ignored on every pass and
                // this pump renewed forever for a consumer that was gone. See the
                // module docs.
                resign = resigns.recv() => {
                    let Some(responder) = resign else {
                        // The consumer dropped the watch without resigning. Stop
                        // renewing — that is what makes the claim lapse at its
                        // deadline, which is `LeaderWatch`'s documented contract —
                        // and the teardown below best-effort resigns so a
                        // successor is elected promptly rather than a TTL later.
                        // Exactly what the in-process pump's `None => break` does
                        // (`cluster/src/defaults/leader.rs`), which is what
                        // invariant I1 asks of this arm.
                        break;
                    };
                    let outcome = match &self.token {
                        Some(token) => self.backend.resign(token).await,
                        // Resigning as a follower is a no-op that succeeds: there
                        // is no claim to give up, which is the same answer an
                        // already-lapsed claim gets (§6.10).
                        None => Ok(()),
                    };
                    responder.respond(outcome);
                    // The claim has just been given back explicitly, so clearing
                    // the token is what keeps the teardown below a no-op rather
                    // than a second resign of the same lease.
                    self.token = None;
                    break;
                }
            }
        }
        // **One exit, one best-effort resign** — the shape Profile 1 has always
        // had (`defaults/leader.rs`, "Teardown (consumer gone / cache watch
        // closed / fatal)"). Profile 3 used to `return` straight out of four
        // arms, so a non-retryable renewal error, a `send_status` failure right
        // after winning, and a terminal subscription close each abandoned a
        // *held* claim to its deadline. Guarded by the token, which is `Some`
        // only for a participant that actually won, so this never resigns a
        // claim the pump did not hold; and `resign` is predicated on
        // `(name, owner, fence)`, so a stale token cannot disturb a successor
        // (§6.10).
        self.release_if_holder().await;
    }

    /// Re-opens the subscription against the **same** `election_id`.
    ///
    /// Returns `false` only when the consumer has gone away.
    async fn reattach(
        &mut self,
        stream: &mut Option<Streaming<stubs::LeaderWatchEventDto>>,
        reattach: &mut Option<Reattach>,
        bound: Duration,
    ) -> bool {
        let Some(state) = reattach.as_mut() else {
            return true;
        };
        // Bounded on purpose: `await_change` carries no RPC deadline (§6.10), and
        // this call runs inside the `select!` handler, so a server that accepts
        // the request and never answers would freeze the renewal timer with it.
        // One renewal interval is the largest bound that cannot cost a tick.
        let opened = tokio::time::timeout(bound, self.backend.subscribe(&self.election_id)).await;
        match opened {
            Ok(Ok(reopened)) => {
                *stream = Some(reopened);
                *reattach = None;
                tracing::debug!(
                    election = %self.name,
                    "cluster: election subscription re-attached"
                );
                // §6.8's `Reset` is exactly this — "the server's upstream
                // subscription was re-established" — and it is the same event
                // ADR-003 has `RestartingWatch` synthesise on every successful
                // resubscribe. Profile 1 forwards `Reset` on a `LeaderWatch` too,
                // so the consumer vocabulary is unchanged in either profile (I1).
                self.sender.send(LeaderWatchEvent::Reset).await.is_ok()
            }
            // A timeout, or a retryable transport error, with budget left.
            Ok(Err(error)) if error.is_retryable() && state.next().is_some() => true,
            Err(_elapsed) if state.next().is_some() => true,
            Ok(Err(error)) => {
                self.abandon_the_feed(reattach, &error.to_string());
                true
            }
            Err(_spent) => {
                self.abandon_the_feed(reattach, "the re-attach budget is spent");
                true
            }
        }
    }

    /// Stops trying to re-attach, and keeps renewing anyway.
    ///
    /// Either the budget is spent or the server answered non-retryably — a swept
    /// or unknown `election_id`, i.e. the subscription is provably gone and no
    /// re-`attach` will ever succeed. **This is not terminal for the pump.** The
    /// claim is a row in the store that only these renewals sustain (invariants
    /// I7, I8), so stopping here would be the very bug this change removes; the
    /// pump carries on blind, exactly as Profile 1 does once its cache watch has
    /// ended (`defaults/leader.rs`, `None => cache_watch = None`).
    fn abandon_the_feed(&self, reattach: &mut Option<Reattach>, why: &str) {
        *reattach = None;
        tracing::warn!(
            election = %self.name,
            reason = why,
            "cluster: giving up on the election subscription; the claim is still \
             being renewed, but this participant will no longer observe \
             server-originated events"
        );
    }

    /// One renewal (as leader) or one re-claim attempt (as follower).
    ///
    /// Returns `false` when the pump must stop.
    async fn tick(&mut self) -> bool {
        let Some(token) = self.token.clone().filter(|_| self.am_leader) else {
            return self.claim().await;
        };
        match self.backend.renew(&token, self.config.ttl()).await {
            Ok(()) => {
                self.missed = 0;
                true
            }
            // The predicate matched nothing: lapsed, or stolen by a successor
            // that fenced this claim out. Both mean leadership is gone, and both
            // are a *status* change — the subscription stays open and this
            // participant may win the next round (§6.6).
            Err(ClusterError::LockExpired { .. }) => self.lose_then_reclaim().await,
            // A transport failure is no evidence about the lease. Keep renewing;
            // only the budget running out ends the claim, which is what makes the
            // deadline — not the network — the thing that decides.
            Err(error) if error.is_retryable() => {
                self.missed = self.missed.saturating_add(1);
                if self.missed <= self.config.max_missed_renewals() {
                    tracing::warn!(
                        election = %self.name,
                        missed = self.missed,
                        %error,
                        "cluster: election renewal failed; retrying"
                    );
                    return true;
                }
                self.lose_then_reclaim().await
            }
            Err(error) => self.close(error).await,
        }
    }

    /// Emits the loss, then tries to take the claim straight back.
    ///
    /// Both halves matter: surfacing the loss is what stops a consumer's
    /// leader-only work, and the immediate re-claim is what makes a self-inflicted
    /// lapse (a slow renewal) recover in one tick instead of one interval.
    async fn lose_then_reclaim(&mut self) -> bool {
        self.token = None;
        self.am_leader = false;
        self.missed = 0;
        if self.sender.send_status(LeaderStatus::Lost).await.is_err() {
            return false;
        }
        self.claim().await
    }

    /// A follower's attempt to take a vacant or lapsed claim.
    async fn claim(&mut self) -> bool {
        match self.backend.join_once(&self.name, self.config).await {
            Ok(joined) if matches!(joined.initial_status.into(), LeaderStatus::Leader) => {
                self.token = Some(LeaseToken::from(joined.token));
                self.am_leader = true;
                self.missed = 0;
                self.sender.send_status(LeaderStatus::Leader).await.is_ok()
            }
            // Someone else holds a live claim. An ordinary outcome, and not an
            // event: a follower that was already a follower has not transitioned.
            Ok(_) => true,
            Err(error) if error.is_retryable() => {
                tracing::debug!(
                    election = %self.name,
                    %error,
                    "cluster: election re-claim failed; retrying on the next tick"
                );
                true
            }
            Err(error) => self.close(error).await,
        }
    }

    /// Forwards one server-originated frame.
    async fn forward(
        &mut self,
        frame: Option<Result<stubs::LeaderWatchEventDto, tonic::Status>>,
    ) -> Pumped {
        match decode_frame(frame) {
            // A `Status` must go through `send_status` so the cached snapshot
            // `LeaderWatch::status()` reads stays coherent with the event stream.
            // The only status the server originates is the `Lost` that precedes a
            // drain (§4.8).
            LeaderWatchEvent::Status(status) => {
                if matches!(status, LeaderStatus::Lost) {
                    self.token = None;
                    self.am_leader = false;
                }
                Pumped::from(self.sender.send_status(status).await.is_ok())
            }
            // **The two signal paths part here.** ADR-003: *"A
            // `Closed(ConnectionLost)` on a `LeaderWatch` is a subscription
            // event. State validity is determined by the renewal-task path"*; and
            // §6.6: *"losing it costs a re-subscribe, not a leadership change"*.
            //
            // Retryability is the discriminator, and it is the one the design
            // already uses for exactly this stream — `RestartingWatch` reads it
            // off `ProviderErrorKind` (ADR-003, §6.9), which is precisely why
            // §6.9 insists the kind travels explicitly rather than being inferred
            // from the canonical variant. A broken stream is
            // `Provider{ConnectionLost}` and is a blip; the server's drain is
            // `Shutdown` and the swept or unknown `election_id` is `NotFound`,
            // and neither is retryable, so both still stop the pump exactly as
            // before.
            LeaderWatchEvent::Closed(error) if error.is_retryable() => {
                tracing::debug!(
                    election = %self.name,
                    %error,
                    "cluster: election subscription lost; re-attaching against the \
                     same election_id, the claim is untouched"
                );
                Pumped::Resubscribe
            }
            LeaderWatchEvent::Closed(error) => {
                let _delivered = self.sender.send(LeaderWatchEvent::Closed(error)).await;
                Pumped::Stop
            }
            other => Pumped::from(self.sender.send(other).await.is_ok()),
        }
    }

    /// Reports a terminal failure and stops. Always `false`.
    async fn close(&mut self, error: ClusterError) -> bool {
        let _delivered = self.sender.send(LeaderWatchEvent::Closed(error)).await;
        false
    }

    /// Gives the claim back when the consumer vanished — before it could be used,
    /// or later by dropping the watch.
    ///
    /// Best-effort on purpose. The claim's real safety net is its deadline: this
    /// pump has already stopped renewing by the time it calls this, so a failed
    /// resign costs a successor one TTL rather than the name forever.
    async fn release_if_holder(&self) {
        if let Some(token) = &self.token {
            let _best_effort = self.backend.resign(token).await;
        }
    }
}

/// What one inbound frame means for the pump.
///
/// Three outcomes rather than the `bool` this used to be, because the middle one
/// is the whole of `ADR-003`'s "independent signal paths": a dead *subscription*
/// is not a dead *claim*, and collapsing the two into "stop" is what made losing
/// the feed cost the election.
enum Pumped {
    /// Keep going; the feed is intact.
    Continue,
    /// The subscription ended and the claim did not. Re-attach; keep renewing.
    Resubscribe,
    /// Terminal — the consumer is gone, or the server said so non-retryably.
    Stop,
}

impl From<bool> for Pumped {
    /// `false` is always "the consumer's channel is closed" at the call sites
    /// that produce it, which is terminal for the pump.
    fn from(delivered: bool) -> Self {
        if delivered {
            Self::Continue
        } else {
            Self::Stop
        }
    }
}

/// The bounded re-attach schedule owed while the feed is down.
struct Reattach {
    /// Index into [`REATTACH_BACKOFF`] for the *next* attempt.
    step: usize,
    /// When the pending attempt is due.
    at: tokio::time::Instant,
}

impl Reattach {
    /// The first attempt, one backoff step from now.
    fn start() -> Self {
        Self {
            step: 1,
            at: tokio::time::Instant::now() + REATTACH_BACKOFF[0],
        }
    }

    fn at(&self) -> tokio::time::Instant {
        self.at
    }

    /// Schedules the next attempt, or `None` when the budget is spent.
    fn next(&mut self) -> Option<()> {
        let delay = *REATTACH_BACKOFF.get(self.step)?;
        self.step += 1;
        self.at = tokio::time::Instant::now() + delay;
        Some(())
    }
}

/// One inbound frame as the union event it stands for (§6.8).
fn decode_frame(
    frame: Option<Result<stubs::LeaderWatchEventDto, tonic::Status>>,
) -> LeaderWatchEvent {
    match frame {
        Some(Ok(proto)) => match decode::<dto::LeaderWatchEventDto, _>(proto) {
            Ok(dto) => to_leader_watch_event(dto),
            // A frame this build cannot decode is a gap, not a dead election.
            Err(error) => {
                tracing::warn!(%error, "cluster: undecodable election frame");
                LeaderWatchEvent::Reset
            }
        },
        Some(Err(status)) => LeaderWatchEvent::Closed(from_subscription_status(&status)),
        // The server closed the subscription without a terminal event — a dropped
        // reader, or the §5.4.1 sweep. Synthesised as the retryable transport
        // failure it is, so `forward` re-attaches rather than stopping; the claim
        // is untouched either way, because it lives in the store and only the
        // pump's renewals sustain it (invariants I7, I8).
        None => LeaderWatchEvent::Closed(crate::convert::transport_failure(
            "the cluster election subscription ended",
        )),
    }
}

/// The next frame, or a future that never completes when there is no feed.
///
/// Profile 1's `recv_optional` (`cluster/src/defaults/leader.rs`), for the same
/// reason: a pump between subscriptions must keep renewing, not stall.
async fn next_frame(
    stream: &mut Option<Streaming<stubs::LeaderWatchEventDto>>,
) -> Option<Result<stubs::LeaderWatchEventDto, tonic::Status>> {
    match stream.as_mut() {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

/// Sleeps until `at`, or never when nothing is owed.
async fn due(at: Option<tokio::time::Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
