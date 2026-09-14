//! Tests for the distributed-lock service.
//!
//! The service holds no lease state, so almost everything here is really a
//! question about the *token*: who may present it, what a foreign one gets, and
//! whether a fenced-out one can still do damage.

use cluster_sdk::grpc::stubs::lock as stubs;
use cluster_sdk::grpc::stubs::lock::distributed_lock_api_server::DistributedLockApi as _;

use super::super::test_harness::{Harness, request};
use super::DistributedLockService;

fn try_lock(profile: &str, name: &str, ttl_ms: u64) -> stubs::TryLockRequest {
    stubs::TryLockRequest {
        profile: profile.to_owned(),
        name: name.to_owned(),
        ttl_ms,
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

#[tokio::test]
async fn the_lock_service_acquires_renews_and_releases() {
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    let acquired = service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("the lock is free")
        .into_inner();
    let token = acquired.token.expect("an acquisition mints a token");
    assert_eq!(token.name, "ledger");
    assert_eq!(token.fence, 1, "a fence counts from one");

    service
        .renew(request(lease_ref("orders", token.clone(), Some(30_000))))
        .await
        .expect("the holder renews its own lease");

    service
        .release(request(lease_ref("orders", token, None)))
        .await
        .expect("the holder releases its own lease");

    // And the lock is free again.
    service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("the released lock is acquirable");

    harness.stop().await;
}

#[tokio::test]
async fn a_held_lock_is_contended_and_travels_as_aborted() {
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("the first acquisition wins");

    let status = service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect_err("a live lease is held");
    assert_eq!(status.code(), tonic::Code::Aborted);

    harness.stop().await;
}

#[tokio::test]
async fn a_blocking_lock_times_out_as_deadline_exceeded() {
    // The wait is the backend's, not this service's - see the module docs on why
    // that is load-bearing rather than tidy. What is asserted here is that the
    // outcome reaches the caller as the variant DESIGN section 6.9 specifies.
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("the first acquisition wins");

    let status = service
        .lock(request(stubs::LockRequest {
            profile: "orders".to_owned(),
            name: "ledger".to_owned(),
            ttl_ms: 30_000,
            timeout_ms: 50,
            client_request_id: None,
        }))
        .await
        .expect_err("the incumbent outlives the timeout");
    assert_eq!(status.code(), tonic::Code::DeadlineExceeded);

    harness.stop().await;
}

#[tokio::test]
async fn a_foreign_token_cannot_renew_and_learns_nothing_from_trying() {
    // The one authorization decision this service owns (DESIGN section 4.6). The
    // answer is `LockExpired`, which is exactly what a token matching nothing
    // gets - so `renew` cannot be used to discover that a live lease exists under
    // another owner.
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    let acquired = service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("the lock is free")
        .into_inner();
    let real = acquired.token.expect("a token");

    // A token naming a live lease, under an owner this caller did not mint.
    let forged = stubs::LeaseToken {
        name: real.name.clone(),
        owner: "api-gateway/3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_owned(),
        fence: real.fence,
    };
    let against_live = service
        .renew(request(lease_ref("orders", forged, Some(30_000))))
        .await
        .expect_err("a foreign token renews nothing");

    // A token naming no lease at all.
    let against_nothing = service
        .renew(request(lease_ref(
            "orders",
            stubs::LeaseToken {
                name: "never-locked".to_owned(),
                owner: "api-gateway/3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_owned(),
                fence: 1,
            },
            Some(30_000),
        )))
        .await
        .expect_err("an unknown token renews nothing");

    assert_eq!(against_live.code(), against_nothing.code());
    assert_eq!(against_live.code(), tonic::Code::FailedPrecondition);

    // And the live lease is untouched: its real holder still renews.
    service
        .renew(request(lease_ref("orders", real, Some(30_000))))
        .await
        .expect("the real holder is unaffected");

    harness.stop().await;
}

#[tokio::test]
async fn an_unauthorized_release_is_an_ok_that_does_nothing() {
    // Section 12.6, verbatim. Never `NotFound`, never `PermissionDenied`: both
    // answers - "released" and "there was nothing of yours to release" - have to
    // be indistinguishable, or `release` becomes a probe.
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    let acquired = service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("the lock is free")
        .into_inner();
    let real = acquired.token.expect("a token");

    service
        .release(request(lease_ref(
            "orders",
            stubs::LeaseToken {
                name: real.name.clone(),
                owner: "api-gateway/3f2504e0-4f89-11d3-9a0c-0305e82c3301".to_owned(),
                fence: real.fence,
            },
            None,
        )))
        .await
        .expect("an unauthorized release is Ok");

    // It did nothing: the lock is still held.
    let status = service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect_err("the foreign release must not have freed the lock");
    assert_eq!(status.code(), tonic::Code::Aborted);

    harness.stop().await;
}

#[tokio::test]
async fn releasing_an_absent_lease_is_ok() {
    // Idempotent by absence (section 6.10): a retried release, or one bearing a
    // token fenced out by a successor, has already achieved what the caller
    // wanted.
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    let acquired = service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("the lock is free")
        .into_inner();
    let token = acquired.token.expect("a token");

    service
        .release(request(lease_ref("orders", token.clone(), None)))
        .await
        .expect("the first release succeeds");
    service
        .release(request(lease_ref("orders", token, None)))
        .await
        .expect("and so does the retry");

    harness.stop().await;
}

#[tokio::test]
async fn a_renewal_without_a_ttl_is_invalid_argument() {
    // The backend stores a deadline, not a duration, so there is no "the previous
    // TTL" to reach for. Rejecting at the boundary names the field.
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    let acquired = service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("the lock is free")
        .into_inner();

    let status = service
        .renew(request(lease_ref(
            "orders",
            acquired.token.expect("a token"),
            None,
        )))
        .await
        .expect_err("a renewal must name a TTL");
    assert_eq!(status.code(), tonic::Code::InvalidArgument);

    harness.stop().await;
}

#[tokio::test]
async fn an_unknown_profile_is_the_not_found_mapped_profile_not_bound() {
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    let status = service
        .try_lock(request(try_lock("not-a-profile", "ledger", 30_000)))
        .await
        .expect_err("an unbound profile is refused");
    assert_eq!(status.code(), tonic::Code::NotFound);

    harness.stop().await;
}

#[tokio::test]
async fn a_reacquired_lease_fences_its_predecessor() {
    // Not a property of this service, but the property this service's whole shape
    // rests on: the token is the authority precisely because a steal bumps
    // `fence`, so a predecessor's token can never match again (invariant I7).
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    let first = service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("the lock is free")
        .into_inner()
        .token
        .expect("a token");

    service
        .release(request(lease_ref("orders", first.clone(), None)))
        .await
        .expect("released");

    let second = service
        .try_lock(request(try_lock("orders", "ledger", 30_000)))
        .await
        .expect("reacquired")
        .into_inner()
        .token
        .expect("a token");

    // The predecessor's token is stale, and its release leaves the successor's
    // lease alone.
    service
        .release(request(lease_ref("orders", first, None)))
        .await
        .expect("a stale release is Ok");
    service
        .renew(request(lease_ref("orders", second, Some(30_000))))
        .await
        .expect("the successor's lease is untouched");

    harness.stop().await;
}

// H8: names validated at the wire boundary

/// H8 verify (1): a lock name the in-process facade rejects
/// (`validate_cluster_name`) is rejected on the wire too, on every
/// name-bearing method, with the contract's `InvalidName` — not a
/// provider-flavoured error and not a silent accept.
#[tokio::test]
async fn an_invalid_lock_name_is_rejected_on_the_wire() {
    // A space is outside `CLUSTER_NAME_RULE` — the facade rejects it, so the
    // wire must too.
    const BAD: &str = "not a name";

    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    let try_lock_err = service
        .try_lock(request(try_lock("orders", BAD, 30_000)))
        .await
        .expect_err("an invalid name must be refused at the boundary");
    assert_eq!(try_lock_err.code(), tonic::Code::InvalidArgument);
    assert!(
        matches!(
            cluster_sdk::convert::from_status(&try_lock_err),
            cluster_sdk::ClusterError::InvalidName { .. }
        ),
        "the wire error must reconstruct as `InvalidName`, the contract's rule"
    );

    let lock_err = service
        .lock(request(stubs::LockRequest {
            profile: "orders".to_owned(),
            name: BAD.to_owned(),
            ttl_ms: 30_000,
            timeout_ms: 50,
            client_request_id: None,
        }))
        .await
        .expect_err("blocking acquire validates the name too");
    assert_eq!(lock_err.code(), tonic::Code::InvalidArgument);

    harness.stop().await;
}

/// H8 verify (2), invariant I1: the same input yields the same error variant in
/// Profile 1 (the facade's `validate_cluster_name`) and Profile 3 (the wire,
/// reconstructed through the client's own `from_status` codec). This is the
/// assertion that could not be written before the server validated at all.
#[tokio::test]
async fn the_wire_and_the_facade_reject_a_bad_lock_name_alike() {
    const BAD: &str = "not a name";

    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    // Profile 1: exactly the code `LockV1::try_lock` runs before the backend.
    let profile_1 =
        cluster_sdk::validate_cluster_name(BAD).expect_err("the facade rejects this name");
    assert!(matches!(
        profile_1,
        cluster_sdk::ClusterError::InvalidName { .. }
    ));

    // Profile 3: the wire status, reconstructed as the remote backend would.
    let wire = service
        .try_lock(request(try_lock("orders", BAD, 30_000)))
        .await
        .expect_err("the wire rejects it too");
    let profile_3 = cluster_sdk::convert::from_status(&wire);

    assert_eq!(
        std::mem::discriminant(&profile_1),
        std::mem::discriminant(&profile_3),
        "Profile 1 and Profile 3 must agree on the error variant (I1)"
    );

    harness.stop().await;
}

// M3: ttl / timeout clamped server-side

/// M3, end to end: a wire caller passing an absurd `ttl_ms` gets a lease whose
/// stored deadline is the ceiling, not the absurd value. The token carries no
/// deadline, so the clamp is read off the lease record the backend wrote — under
/// the raw handle, since the reserved keyspace is unreadable through the cache
/// API (B2).
#[tokio::test]
async fn a_wire_ttl_beyond_the_ceiling_is_clamped() {
    let harness = Harness::wired(&["orders"]).await;
    let service = DistributedLockService::new(harness.ctx.clone());

    let before = cluster_sdk::lease::LeaseClock::new().now_millis();
    service
        .try_lock(request(try_lock("orders", "ledger", u64::MAX)))
        .await
        .expect("the lock is free");

    let bound = harness.registry.resolve("orders").expect("orders is bound");
    let entry = bound
        .cache
        .get("$lease/lock/ledger")
        .await
        .expect("reading the raw lease store succeeds")
        .expect("the acquire wrote a lease record");
    let record = cluster_sdk::lease::LeaseRecord::decode(&entry.value).expect("the record decodes");

    let ceiling_ms = u64::try_from(crate::api::grpc::MAX_LEASE_TTL.as_millis()).unwrap();
    // Unclamped, `deadline_after(u64::MAX ms)` saturates to `u64::MAX`; clamped,
    // it sits ~one ceiling from now. A generous upper bound cleanly separates the
    // two, and a lower bound proves it clamped *to the ceiling* rather than to
    // something small.
    assert!(
        record.deadline_ms <= before + ceiling_ms + 60_000,
        "deadline {} was not clamped to the ceiling (~{} from {})",
        record.deadline_ms,
        ceiling_ms,
        before
    );
    assert!(
        record.deadline_ms >= before + ceiling_ms - 60_000,
        "deadline {} is below the ceiling — clamped to the wrong value",
        record.deadline_ms
    );

    harness.stop().await;
}

/// M3, boundary values: the clamp is the identity below the ceiling and pins to
/// it at and above. Covers `timeout_ms`, which cannot be observed end to end
/// without waiting out the ceiling.
#[test]
fn the_ttl_and_timeout_clamps_hold_at_the_boundary() {
    use std::time::Duration;

    let ceiling = crate::api::grpc::MAX_LEASE_TTL;
    let ceiling_ms = u64::try_from(ceiling.as_millis()).unwrap();

    // Zero is meaningless and rejected, matching the election path.
    assert!(super::checked_ttl(0).is_err());
    // Below the ceiling: passed through untouched — no legitimate caller moves.
    assert_eq!(super::checked_ttl(30_000).unwrap(), Duration::from_secs(30));
    assert_eq!(
        super::checked_ttl(ceiling_ms - 1).unwrap(),
        Duration::from_millis(ceiling_ms - 1)
    );
    // At and above: pinned to the ceiling.
    assert_eq!(super::checked_ttl(ceiling_ms).unwrap(), ceiling);
    assert_eq!(super::checked_ttl(ceiling_ms + 1).unwrap(), ceiling);
    assert_eq!(super::checked_ttl(u64::MAX).unwrap(), ceiling);

    let t_ceiling = super::MAX_LOCK_TIMEOUT;
    let t_ceiling_ms = u64::try_from(t_ceiling.as_millis()).unwrap();
    assert_eq!(super::clamped_timeout(50), Duration::from_millis(50));
    assert_eq!(super::clamped_timeout(t_ceiling_ms), t_ceiling);
    assert_eq!(super::clamped_timeout(u64::MAX), t_ceiling);
}
