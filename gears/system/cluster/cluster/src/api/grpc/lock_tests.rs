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
