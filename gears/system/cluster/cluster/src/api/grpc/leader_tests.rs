//! Tests for the leader-election service.
//!
//! Two properties carry most of the weight. **A claim is a lease**, so everything
//! the lock tests assert about tokens holds here too and is not re-asserted.
//! **A subscription is not a lease**, which is asserted directly: the renewal that
//! holds leadership must survive the subscription being dropped.

use std::time::Duration;

use cluster_sdk::grpc::stubs::leader as stubs;
use cluster_sdk::grpc::stubs::leader::leader_election_api_server::LeaderElectionApi as _;
use cluster_sdk::leader::{LeaderStatus, LeaderWatchEvent};

use super::super::test_harness::{Harness, request};
use super::LeaderElectionService;
use crate::api::grpc::subscriptions::SubscriptionId;

fn join(profile: &str, name: &str) -> stubs::JoinRequest {
    stubs::JoinRequest {
        profile: profile.to_owned(),
        name: name.to_owned(),
        ttl_ms: 30_000,
        max_missed_renewals: None,
        client_request_id: None,
    }
}

fn lease_ref(profile: &str, token: stubs::LeaseToken, ttl_ms: Option<u64>) -> stubs::LeaseRef {
    stubs::LeaseRef {
        profile: profile.to_owned(),
        token: Some(token),
        ttl_ms,
        client_request_id: None,
    }
}

fn service(harness: &Harness) -> LeaderElectionService {
    LeaderElectionService::new(
        harness.ctx.clone(),
        std::sync::Arc::clone(&harness.subscriptions),
    )
}

#[tokio::test]
async fn joining_an_uncontested_election_wins_the_claim() {
    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let joined = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("join succeeds")
        .into_inner();

    assert_eq!(
        joined.initial_status,
        i32::from(stubs::WireLeaderStatus::Leader)
    );
    let token = joined.token.expect("the winner carries a claim");
    assert_eq!(token.name, "ledger");
    assert!(!joined.election_id.is_empty());

    harness.stop().await;
}

#[tokio::test]
async fn a_follower_gets_an_ordinary_response_and_the_zero_token() {
    // Losing an election is an ordinary outcome, not an error, so it is not one
    // on the wire either. `LeaderJoined.token` is not optional in the DTO, so a
    // follower receives the zero token - and a real fence counts from one, which
    // is what makes zero unambiguous. A client must read `initial_status`.
    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    service
        .join(request(join("orders", "ledger")))
        .await
        .expect("the first candidate wins");

    let joined = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("the second candidate is a follower, not an error")
        .into_inner();

    assert_eq!(
        joined.initial_status,
        i32::from(stubs::WireLeaderStatus::Follower)
    );
    let token = joined.token.expect("the field is present");
    assert_eq!(token.fence, 0, "the zero token, which no predicate matches");
    assert!(token.owner.is_empty());

    // And it still gets a subscription: a shutdown has to reach followers too.
    assert!(!joined.election_id.is_empty());

    harness.stop().await;
}

#[tokio::test]
async fn a_renewal_succeeds_after_the_subscription_is_dropped() {
    // The property that keeps section 5.4's index out of the lease path. Nothing
    // in `renew` reads the subscription table, so dropping the entry cannot cost
    // a leader its claim (invariant I7, and item `S2`'s exit criterion).
    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let joined = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("join succeeds")
        .into_inner();
    let token = joined.token.expect("the winner carries a claim");

    harness
        .subscriptions
        .close(&SubscriptionId::from(joined.election_id));
    assert!(harness.subscriptions.is_empty());

    service
        .renew(request(lease_ref("orders", token, Some(30_000))))
        .await
        .expect("leadership is held by the lease, not by the subscription");

    harness.stop().await;
}

#[tokio::test(start_paused = true)]
async fn a_renewal_succeeds_after_the_sweep_reaps_the_subscription() {
    // Item `S2`'s exit criterion against the mechanism `S2` actually adds. The
    // test above drops the entry by hand; this one lets the abandoned-subscription
    // sweep (section 5.4.1) reap it, which is how it will really disappear - a
    // `join` whose `await_change` never came is exactly the entry the sweep is
    // there for, and the claim that `join` won must be untouched by its removal
    // (invariant I7).
    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let joined = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("join succeeds")
        .into_inner();
    let token = joined.token.expect("the winner carries a claim");
    assert_eq!(harness.subscriptions.len(), 1);

    // Past the 15 s grace window and well short of the claim's own 30 s TTL. The
    // advance is squeezed from both sides and the margins are not symmetric, so
    // it is spelled out rather than left to a round multiple: too little and the
    // sweep has nothing to reap, too much and the *claim* lapses on the virtual
    // clock and the renewal below fails for a reason that has nothing to do with
    // the subscription. `grace * 2` was exactly 30 s and lost that race
    // intermittently under load.
    let grace = crate::api::grpc::sweep_grace(crate::api::grpc::SWEEP_INTERVAL);
    tokio::time::advance(grace + Duration::from_secs(1)).await;
    let report = harness.subscriptions.sweep(grace);

    assert_eq!(
        report.reaped_total(),
        1,
        "never attached, and past its window"
    );
    assert!(harness.subscriptions.is_empty());

    service
        .renew(request(lease_ref("orders", token.clone(), Some(30_000))))
        .await
        .expect("the reaped subscription cost the leader nothing");

    // And it is still the leader afterwards: a second candidate still loses.
    let contender = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("join succeeds")
        .into_inner();
    assert_eq!(
        contender.initial_status,
        i32::from(stubs::WireLeaderStatus::Follower),
        "the claim survived the sweep, so nobody else can take it"
    );

    service
        .resign(request(lease_ref("orders", token, None)))
        .await
        .expect("and it can still give the claim back");

    harness.stop().await;
}

#[tokio::test]
async fn resigning_frees_the_election_for_the_next_candidate() {
    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let token = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("join succeeds")
        .into_inner()
        .token
        .expect("a claim");

    service
        .resign(request(lease_ref("orders", token, None)))
        .await
        .expect("resign succeeds");

    let next = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("join succeeds")
        .into_inner();
    assert_eq!(
        next.initial_status,
        i32::from(stubs::WireLeaderStatus::Leader),
        "the resigned election is winnable again"
    );

    harness.stop().await;
}

#[tokio::test]
async fn an_unauthorized_resign_is_an_ok_that_does_nothing() {
    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let real = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("join succeeds")
        .into_inner()
        .token
        .expect("a claim");

    service
        .resign(request(lease_ref(
            "orders",
            stubs::LeaseToken {
                name: real.name.clone(),
                owner: "api-gateway/3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_owned(),
                fence: real.fence,
            },
            None,
        )))
        .await
        .expect("an unauthorized resign is Ok");

    // The claim is untouched: its real holder still renews.
    service
        .renew(request(lease_ref("orders", real, Some(30_000))))
        .await
        .expect("the leader kept its claim");

    harness.stop().await;
}

#[tokio::test]
async fn an_invalid_election_config_is_rejected_by_the_sdk_s_own_rule() {
    // One validation, one message, both profiles - which invariant I1
    // asks for on the error path as much as the success path.
    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let status = service
        .join(request(stubs::JoinRequest {
            ttl_ms: 0,
            ..join("orders", "ledger")
        }))
        .await
        .expect_err("a zero TTL is rejected");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    let too_wide = service
        .join(request(stubs::JoinRequest {
            max_missed_renewals: Some(u64::from(u16::MAX)),
            ..join("orders", "ledger")
        }))
        .await
        .expect_err("a budget that cannot fit the SDK's byte is rejected");
    assert_eq!(too_wide.code(), tonic::Code::InvalidArgument);

    harness.stop().await;
}

#[tokio::test]
async fn await_change_streams_the_server_originated_events() {
    use tokio_stream::StreamExt as _;

    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let joined = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("join succeeds")
        .into_inner();

    let mut stream = service
        .await_change(request(stubs::AwaitChangeRequest {
            profile: "orders".to_owned(),
            election_id: joined.election_id,
        }))
        .await
        .expect("the subscription opens")
        .into_inner();

    // Item S5's sequence, through the fan-out it will use - the terminal one,
    // whose reserved headroom is what makes the two-step survive a full buffer.
    harness
        .subscriptions
        .broadcast_terminal(&LeaderWatchEvent::Status(LeaderStatus::Lost));
    harness
        .subscriptions
        .broadcast_terminal(&LeaderWatchEvent::Closed(
            cluster_sdk::ClusterError::Shutdown,
        ));

    let lost = stream
        .next()
        .await
        .expect("an event")
        .expect("not an error");
    assert_eq!(lost.kind, i32::from(stubs::LeaderWatchEventKind::Status));
    assert_eq!(lost.status, Some(i32::from(stubs::WireLeaderStatus::Lost)));

    let closed = stream
        .next()
        .await
        .expect("an event")
        .expect("not an error");
    assert_eq!(closed.kind, i32::from(stubs::LeaderWatchEventKind::Closed));
    let error = closed.error.expect("a Closed carries its terminal error");
    assert_eq!(error.error_code, "shutdown");

    assert!(
        stream.next().await.is_none(),
        "Closed is terminal: the server sends it, then closes the stream"
    );

    harness.stop().await;
}

#[tokio::test]
async fn an_unknown_or_foreign_election_id_is_not_found() {
    // Section 6.9's `AwaitChange` row: the client reconstructs
    // `Closed(ClusterError::Shutdown)` from it - terminal and non-retryable, so
    // `RestartingWatch` propagates rather than resubscribing.
    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let status = service
        .await_change(request(stubs::AwaitChangeRequest {
            profile: "orders".to_owned(),
            election_id: SubscriptionId::mint().to_string(),
        }))
        .await
        .expect_err("an unknown subscription is refused");
    assert_eq!(status.code(), tonic::Code::NotFound);

    harness.stop().await;
}

#[tokio::test]
async fn await_change_against_an_unbound_profile_is_profile_not_bound() {
    // The profile is dispatched even though no backend call follows, so a
    // subscription against an unbound profile fails the same way every other
    // request against it does.
    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let status = service
        .await_change(request(stubs::AwaitChangeRequest {
            profile: "not-a-profile".to_owned(),
            election_id: SubscriptionId::mint().to_string(),
        }))
        .await
        .expect_err("an unbound profile is refused");
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert!(
        status.message().contains("no backend bound"),
        "the profile answer must win over the subscription answer: {}",
        status.message()
    );

    harness.stop().await;
}

#[tokio::test]
async fn audit_control_a_cancelled_election_stream_drops_its_receiver() {
    // `WATCH-1`'s control. This pump already selects on `tx.closed()`, so it is
    // the passing half of the pair: the cache pump's failure is a missing arm,
    // not a false expectation about what tonic does on cancellation.
    let (events, rx) = tokio::sync::mpsc::channel::<LeaderWatchEvent>(8);
    let stream = super::subscription_stream(rx);

    drop(stream);

    let released = tokio::time::timeout(Duration::from_secs(5), async {
        while !events.is_closed() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    assert!(
        released.is_ok(),
        "a cancelled election stream must drop the subscription's receiver, which is what \
         `has_live_reader` reads and the sweep acts on"
    );
}

// H8: election names validated at the wire boundary

/// H8 verify (1) + (2): `join` rejects an election name the facade rejects, with
/// the contract's `InvalidName`, and Profile 1 and Profile 3 agree on the
/// variant (I1). `join` is the only name-bearing method here — `renew`/`resign`
/// carry a token, whose name was validated when it was minted.
#[tokio::test]
async fn an_invalid_election_name_is_rejected_on_the_wire() {
    const BAD: &str = "not a name";

    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let profile_1 =
        cluster_sdk::validate_cluster_name(BAD).expect_err("the facade rejects this name");
    assert!(matches!(
        profile_1,
        cluster_sdk::ClusterError::InvalidName { .. }
    ));

    let wire = service
        .join(request(join("orders", BAD)))
        .await
        .expect_err("the wire rejects it too");
    assert_eq!(wire.code(), tonic::Code::InvalidArgument);
    let profile_3 = cluster_sdk::convert::from_status(&wire);
    assert_eq!(
        std::mem::discriminant(&profile_1),
        std::mem::discriminant(&profile_3),
        "Profile 1 and Profile 3 must agree on the error variant (I1)"
    );

    harness.stop().await;
}

// M9 + ttl_ms = 0: lock and election agree, and both ship a trailer

/// The lock and election paths must agree about `ttl_ms = 0`: a zero-TTL lease
/// lapses the instant it is taken, so both reject it — and both reject it
/// *through the codec*, so the status carries a problem trailer and the client
/// reconstructs the typed [`ClusterError::InvalidConfig`] a consumer branches
/// on rather than the opaque `Provider` error a bare `Status` decodes to
/// (M9, the one-codec invariant at `api::grpc::mod`).
///
/// The `from_status` reconstruction is the trailer assertion: without the
/// trailer a hand-built `invalid_argument` would come back as a provider-flavoured
/// error, not `InvalidConfig`.
#[tokio::test]
async fn ttl_zero_is_rejected_alike_on_the_lock_and_election_paths() {
    use cluster_sdk::ClusterError;
    use cluster_sdk::convert::from_status;
    use cluster_sdk::grpc::stubs::lock as lock_stubs;
    use cluster_sdk::grpc::stubs::lock::distributed_lock_api_server::DistributedLockApi as _;

    let harness = Harness::wired(&["orders"]).await;
    let leader = service(&harness);
    let lock = crate::api::grpc::DistributedLockService::new(harness.ctx.clone());

    // Election path: `join` with a zero TTL, rejected in `ElectionConfig::new`.
    let election_status = leader
        .join(request(stubs::JoinRequest {
            ttl_ms: 0,
            ..join("orders", "ledger")
        }))
        .await
        .expect_err("a zero-TTL election is rejected");
    assert_eq!(election_status.code(), tonic::Code::InvalidArgument);
    let election_err = from_status(&election_status);

    // Lock path: `try_lock` with a zero TTL, now rejected in `checked_ttl`.
    let lock_status = lock
        .try_lock(request(lock_stubs::TryLockRequest {
            profile: "orders".to_owned(),
            name: "ledger".to_owned(),
            ttl_ms: 0,
            client_request_id: None,
        }))
        .await
        .expect_err("a zero-TTL lock is rejected");
    assert_eq!(lock_status.code(), tonic::Code::InvalidArgument);
    let lock_err = from_status(&lock_status);

    // Trailer shipped on both: reconstruct as the typed `InvalidConfig`, never a
    // `Provider` catch-all.
    assert!(
        matches!(election_err, ClusterError::InvalidConfig { .. }),
        "the election rejection must carry a trailer, got {election_err:?}"
    );
    assert!(
        matches!(lock_err, ClusterError::InvalidConfig { .. }),
        "the lock rejection must carry a trailer, got {lock_err:?}"
    );

    // Parity: the same input yields the same typed variant on both primitives.
    assert_eq!(
        std::mem::discriminant(&election_err),
        std::mem::discriminant(&lock_err),
        "lock and election must reject `ttl_ms = 0` with the same error variant"
    );

    harness.stop().await;
}

/// Election `renew` clamps and zero-rejects its TTL exactly as `join` and the
/// lock paths do (M3/M9). Before this fix, election `renew` passed the raw wire
/// `ttl_ms` straight to the backend — so `ttl_ms = 0` (an instantly-lapsed lease)
/// and an unbounded TTL were accepted on renewal though the lock renew path and
/// election `join` both reject/clamp them: an acquire-then-renew bypass and an
/// I1 divergence between the two renew paths. The rejection ships through the
/// codec, so the client reconstructs `InvalidConfig`, not a bare `Provider`.
#[tokio::test]
async fn an_election_renewal_rejects_a_zero_ttl_through_the_codec() {
    use cluster_sdk::ClusterError;
    use cluster_sdk::convert::from_status;

    let harness = Harness::wired(&["orders"]).await;
    let service = service(&harness);

    let token = service
        .join(request(join("orders", "ledger")))
        .await
        .expect("join succeeds")
        .into_inner()
        .token
        .expect("the winner carries a claim");

    let status = service
        .renew(request(lease_ref("orders", token, Some(0))))
        .await
        .expect_err("a zero-TTL renewal is rejected, as on the lock path");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert!(
        matches!(from_status(&status), ClusterError::InvalidConfig { .. }),
        "the renewal rejection must ship a trailer, not a bare Status"
    );

    harness.stop().await;
}
