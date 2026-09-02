//! The leader-election service (DESIGN.md).
//!
//! The lock's shape plus one subscription: `join` mints a lease exactly as
//! `try_lock` does, `renew` and `resign` are the same token-predicated writes
//! against the same kind of record, and `await_change` follows one election.
//!
//! # Where §12.6's sketch loses, and why
//!
//! §12.6 writes `await_change` as a **long-poll** over a server-held
//! `LeaderWatch`: `self.sessions.borrow_election(..)` then
//! `timeout(r.timeout(), session.watch.changed())`. Two facts in the tree overrule
//! it, and §6.6 sanctions both projections, so this is the design choosing between
//! its own alternatives rather than a departure from it.
//!
//! **The contract as built is a stream.** `await_change` carries `#[streaming]` on
//! both the contract and the projection (items `C1`/`C2`), so the generated server
//! trait demands an `AwaitChangeStream`. There is no `timeout` field on
//! `AwaitChangeRequest` to long-poll against.
//!
//! **There is no `LeaderWatch` on this path to hold.** The backend's lease half is
//! [`join`](cluster_sdk::LeaderElectionBackend::join), which returns a token and
//! nothing else; the half that returns a `LeaderWatch` is `elect`, and `elect`
//! makes the *backend* renew on the caller's behalf. Holding one here would keep a
//! dead consumer elected, which §7.3 rules out and invariant I8 forbids: renewal
//! is client-driven precisely so that renewal stays the consumer-liveness proxy.
//!
//! # So what does the subscription carry?
//!
//! Exactly the events only the server knows, and no others:
//!
//! | Event | Source | Sent by |
//! |---|---|---|
//! | `Status(Lost)` | this gear draining (§4.8), delivered **before** the close | `ClusterGear::stop`, through [`ElectionSubscriptions::broadcast_terminal`] |
//! | `Closed(Shutdown)` | the same drain, and the last event on the stream | the same call, then `close_all` drops the sender |
//! | `Reset` | the subscription was re-established | nothing on this side: a re-established subscription is a *new* `await_change`, so the client's own `RestartingWatch` synthesizes it |
//!
//! The third column exists because the first two rows used to be aspirational:
//! `broadcast_terminal` had no production caller either, so a Profile 3 stream
//! carried none of these events and never ended at all. Naming the sender is what
//! makes a row checkable. There is deliberately **no** ordinary server-side
//! fan-out on this path — a leadership transition is derived client-side (see
//! below) — so the stream carries only the terminal two-step and the
//! client-synthesized `Reset`, and there is no server event for a subscriber to
//! fall behind: `Lagged` is a client-pump concern, not a row on this table.
//!
//! **A leadership transition is not on that list, and does not need to be.** A
//! holder learns it lost the claim from its own `renew` returning `lock_expired`,
//! which §6.6 states outright — "the pump emits `Status(Lost)` and keeps the
//! subscription open". A follower learns it won by re-`join`ing on its own
//! cadence, which the in-process default's own renewal loop does
//! internally. So the client-side pump (item `K2`, §12.12) derives transitions the
//! same way in both deployment profiles, which keeps invariant I1 true
//! here; the stream carries what a client cannot derive for itself.
//!
//! # A follower's `join` carries no token
//!
//! [`join`](cluster_sdk::LeaderElectionBackend::join) returns `None` when another
//! candidate holds a live claim — an ordinary outcome, not an error, so it is not
//! one on the wire either. `LeaderJoined.token` is not optional in the DTO, so a
//! follower receives the **zero token**: empty name, empty owner, `fence: 0`. A
//! real fence counts from 1, so zero is unambiguous, and every predicate the zero
//! token could reach declines it. **A client must read `initial_status`, never the
//! token's shape.** Making the field optional would be the cleaner surface and is
//! a `C1`/`C2` change (the proto field is already `optional`, so no field number
//! moves); it is recorded rather than made here, because the wire is `C1`'s to
//! move and nothing has deployed against either shape yet.

use cluster_sdk::dto;
use cluster_sdk::grpc::stubs::leader as stubs;
use cluster_sdk::leader::{ElectionConfig, LeaderStatus, LeaderWatchEvent};
use cluster_sdk::lease::LeaseToken;
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};

use super::subscriptions::{SharedSubscriptions, SubscriptionId};
use super::{ServiceContext, checked_ttl, wire_error};

/// The leader-election primitive, served over the wire.
#[derive(Debug, Clone)]
pub struct LeaderElectionService {
    ctx: ServiceContext,
    subscriptions: SharedSubscriptions,
}

impl LeaderElectionService {
    /// Builds the service over the shared [`ServiceContext`] and the subscription
    /// table.
    ///
    /// The table is passed in rather than owned so item `S5` can broadcast the
    /// shutdown sequence through the same handle, and `S2` can sweep it.
    #[must_use]
    pub fn new(ctx: ServiceContext, subscriptions: SharedSubscriptions) -> Self {
        Self { ctx, subscriptions }
    }

    /// The subscription table, for `S5`'s revoke fan-out and `S2`'s sweep.
    #[must_use]
    pub fn subscriptions(&self) -> &SharedSubscriptions {
        &self.subscriptions
    }

    fn renew_ack(&self) -> stubs::RenewResponse {
        stubs::RenewResponse::from(dto::RenewResponse {
            generation: self.ctx.profiles().generation(),
        })
    }

    fn resign_ack(&self) -> stubs::ResignResponse {
        stubs::ResignResponse::from(dto::ResignResponse {
            generation: self.ctx.profiles().generation(),
        })
    }
}

/// The stream `await_change` returns.
pub type LeaderWatchStream = ReceiverStream<Result<stubs::WireLeaderWatchEvent, Status>>;

#[tonic::async_trait]
impl stubs::leader_election_api_server::LeaderElectionApi for LeaderElectionService {
    async fn join(
        &self,
        request: Request<stubs::JoinRequest>,
    ) -> Result<Response<stubs::LeaderJoined>, Status> {
        let (caller, bound) = self.ctx.authorize(&request, &request.get_ref().profile)?;
        let req = request.into_inner();
        // H8: the facade validates the election name client-side; the server must
        // too, or Profile 3 accepts names Profile 1 rejects (invariant I1). The
        // scope-aware rule is required: `.scoped()` composes `prefix/name`, which
        // a bare `validate_cluster_name` (no `/`) would reject on the wire only.
        cluster_sdk::validate_scoped_cluster_name(&req.name).map_err(cluster_sdk::to_status)?;

        let config = election_config(req.ttl_ms, req.max_missed_renewals)?;
        let claim = bound
            .leader_election
            .join(&req.name, &caller.mint_owner(), config)
            .await
            .map_err(cluster_sdk::to_status)?;

        // The subscription is registered whether or not the claim was won: a
        // follower needs the channel as much as a leader does, since a shutdown
        // must reach both (§4.8) and a follower that later wins re-`join`s rather
        // than being told (see the module docs). The stream that reads it is
        // opened by `await_change`.
        //
        // **This is where the follower pump's leak enters** (§5.4.1): the re-join
        // that is a follower's only route to winning mints one of these every
        // renewal interval, and the pump keeps its original id. The profile is
        // taken from the *resolved* binding rather than the request, so the
        // interned gauge label is the registry's canonical name and nothing
        // caller-supplied reaches the intern table.
        let id = self
            .subscriptions
            .open(caller.name(), &req.name, &bound.name);

        Ok(Response::new(stubs::LeaderJoined::from(
            dto::LeaderJoined {
                initial_status: if claim.is_some() {
                    dto::WireLeaderStatus::from(LeaderStatus::Leader)
                } else {
                    dto::WireLeaderStatus::from(LeaderStatus::Follower)
                },
                token: claim.map(dto::LeaseToken::from).unwrap_or_default(),
                election_id: id.to_string(),
            },
        )))
    }

    async fn renew(
        &self,
        request: Request<stubs::LeaseRef>,
    ) -> Result<Response<stubs::RenewResponse>, Status> {
        let (caller, bound) = self.ctx.authorize(&request, &request.get_ref().profile)?;
        let lease = dto::LeaseRef::from(request.into_inner());
        let token = LeaseToken::from(lease.token);

        // Same rule as the lock's renew, and the same reason: indistinguishable
        // from a claim that lapsed, was stolen, or was never this caller's (§6.9).
        if !caller.owns(&token) {
            return Err(cluster_sdk::to_status(
                cluster_sdk::ClusterError::LockExpired { name: token.name },
            ));
        }

        let ttl = lease.ttl_ms.ok_or_else(|| {
            // Routed through the codec, not a bare `Status`, so the rejection
            // ships a problem trailer and the client reconstructs a typed
            // `InvalidConfig` rather than `Provider{Other}` (M9, one-codec
            // invariant at `api::grpc::mod`).
            cluster_sdk::to_status(cluster_sdk::ClusterError::InvalidConfig {
                reason: "an election renewal must carry `ttl_ms`".to_owned(),
            })
        })?;

        // **The operation that holds leadership** (§7.3), and it reads nothing
        // from the subscription table — item `S2`'s exit criterion in one line.
        bound
            .leader_election
            // M3/M9: clamped and zero-rejected exactly as the lock renew path
            // and election `join` are, so the acquire-then-renew bypass is closed
            // on both primitives (invariant I1).
            .renew(&token, checked_ttl(ttl)?)
            .await
            .map_err(cluster_sdk::to_status)?;

        Ok(Response::new(self.renew_ack()))
    }

    async fn resign(
        &self,
        request: Request<stubs::LeaseRef>,
    ) -> Result<Response<stubs::ResignResponse>, Status> {
        let (caller, bound) = self.ctx.authorize(&request, &request.get_ref().profile)?;
        let lease = dto::LeaseRef::from(request.into_inner());
        let token = LeaseToken::from(lease.token);

        if caller.owns(&token) {
            bound
                .leader_election
                .resign(&token)
                .await
                .map_err(cluster_sdk::to_status)?;
        }

        Ok(Response::new(self.resign_ack()))
    }

    type AwaitChangeStream = LeaderWatchStream;

    /// Follows one election's server-originated transitions.
    ///
    /// An unknown `election_id` — including one belonging to another caller — is
    /// `NotFound`, which the client's codec reconstructs as
    /// `Closed(ClusterError::Shutdown)`: terminal and non-retryable, so
    /// `RestartingWatch` propagates rather than resubscribing, and the consumer's
    /// recovery is an explicit re-`elect` (§6.9's `AwaitChange` row).
    ///
    /// **Carries no RPC timeout**, because §7.3 puts liveness on the consumer's
    /// renewal rather than on the transport: "the transport owes no keepalive …
    /// a subscription is an observation channel and nothing more".
    ///
    /// Two things this comment used to say, both false. No HTTP/2 keepalive is
    /// configured anywhere in the cluster tree or the toolkit gRPC path, so the
    /// rule cannot rest on one. And a deadline does *not* in fact sever a tonic
    /// 0.14 stream — measured, not inferred. The rule stands; only
    /// its stated justification was wrong. The consequence worth carrying: an
    /// election subscription has no liveness mechanism of its own, so a half-open
    /// connection leaves the feed dead with both sides believing it live.
    async fn await_change(
        &self,
        request: Request<stubs::AwaitChangeRequest>,
    ) -> Result<Response<Self::AwaitChangeStream>, Status> {
        // The profile is dispatched even though no backend call follows it: a
        // subscription against an unbound profile must fail the same way every
        // other request against it does, or the two answers diverge across a
        // reload (§5.6).
        let (caller, _bound) = self.ctx.authorize(&request, &request.get_ref().profile)?;
        let req = request.into_inner();
        let id = SubscriptionId::from(req.election_id);

        // Attaching keeps the id and swaps in the reader this stream will serve,
        // which makes a reconnect after a broken stream work without a
        // fresh `join`, and what enforces §6.6's one-reader rule.
        let events = self
            .subscriptions
            .attach(&id, caller.name())
            .ok_or_else(|| Status::not_found("unknown election_id"))?;

        Ok(Response::new(subscription_stream(events)))
    }
}

/// Turns the subscription's receiver into the gRPC stream.
///
/// One send per event and no buffering of its own: the bounded buffer and the
/// drop-then-`Lagged` rule live in [`ElectionSubscriptions`], which
/// broadcasts into it.
///
/// # Departure is watched for, not waited on
///
/// The loop selects on `tx.closed()` as well as the next event, and that arm is
/// what makes the sweep able to see an abandoned subscription at all (§5.4.1).
/// Parked on `recv()` alone, this task would hold `events` alive until the *next*
/// event arrived — which for a quiet election is never — so the table's stored
/// sender would keep reporting a live reader long after the client had gone. With
/// the arm, a cancelled stream or a dead peer drops the receiver promptly, the
/// entry becomes unread, and its grace window starts.
///
/// The entry itself is deliberately **not** removed here: a client's
/// `election_id` stays valid across a broken stream, so a reconnect inside the
/// window needs no fresh `join` (§6.6). The sweep is what decides it is over.
///
/// [`ElectionSubscriptions`]: super::subscriptions::ElectionSubscriptions
fn subscription_stream(
    mut events: tokio::sync::mpsc::Receiver<LeaderWatchEvent>,
) -> LeaderWatchStream {
    let (tx, rx) = tokio::sync::mpsc::channel(1);

    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                event = events.recv() => event,
                // The subscriber went away; cancelling the stream is
                // unsubscribing, exactly as dropping a `LeaderWatch` is
                // in-process.
                () = tx.closed() => return,
            };
            let Some(event) = event else { return };

            let terminal = matches!(event, LeaderWatchEvent::Closed(_));
            if tx.send(Ok(to_dto(event))).await.is_err() {
                return;
            }
            if terminal {
                return;
            }
        }
    });

    ReceiverStream::new(rx)
}

/// The election timing the caller asked for, validated by the SDK's own rules.
///
/// `ElectionConfig::new` is the single place those rules live
/// (`cpt-cf-clst-algo-leader-election-config-validate`), so a remote caller and an
/// in-process one are rejected by the same code with the same message — which is
/// what invariant I1 asks for on the error path as much as the success path.
fn election_config(
    ttl_ms: u64,
    max_missed_renewals: Option<u64>,
) -> Result<ElectionConfig, Status> {
    let budget = match max_missed_renewals {
        None => ElectionConfig::DEFAULT_MAX_MISSED_RENEWALS,
        Some(value) => u8::try_from(value).map_err(|_| {
            // Through the codec, not a bare `Status`, so the rejection ships a
            // problem trailer and reconstructs as a typed `InvalidConfig` rather
            // than `Provider{Other}` (M9, one-codec invariant).
            cluster_sdk::to_status(cluster_sdk::ClusterError::InvalidConfig {
                reason: format!("max_missed_renewals must fit in a byte (got {value})"),
            })
        })?,
    };
    ElectionConfig::new(checked_ttl(ttl_ms)?, budget).map_err(cluster_sdk::to_status)
}

/// A leader watch-union event becomes its flat wire form (§6.8).
#[allow(
    clippy::match_same_arms,
    reason = "as in the cache union: `Reset` is the backend re-establishing the subscription, the wildcard is an event kind this build does not know. Same value, different facts"
)]
fn to_dto(event: LeaderWatchEvent) -> stubs::WireLeaderWatchEvent {
    let dto = match event {
        LeaderWatchEvent::Status(status) => {
            dto::WireLeaderWatchEvent::status(dto::WireLeaderStatus::from(status))
        }
        LeaderWatchEvent::Lagged { dropped } => dto::WireLeaderWatchEvent::lagged(dropped),
        LeaderWatchEvent::Reset => dto::WireLeaderWatchEvent::reset(),
        LeaderWatchEvent::Closed(error) => dto::WireLeaderWatchEvent::closed(wire_error(error)),
        // Required, not chosen: the enum is `#[non_exhaustive]` and defined in
        // another crate. `Reset` for the same reason as the cache union — never
        // claim leadership from an event this build does not understand, and
        // never drop it silently (§6.8).
        _ => dto::WireLeaderWatchEvent::reset(),
    };
    stubs::WireLeaderWatchEvent::from(dto)
}

#[cfg(test)]
#[path = "leader_tests.rs"]
mod leader_tests;
