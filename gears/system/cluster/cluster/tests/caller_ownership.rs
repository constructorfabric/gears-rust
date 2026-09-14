//! The ownership cross-check (`S1`), over a real socket, with the identities a
//! platform-plane `InternalAuthGrpcLayer` actually stamps — the direct H6
//! regression (DESIGN §5.8.1, §12.6).
//!
//! The backend's lease methods are token-only, so "is the transport caller the
//! token's owner" is the serving gear's one authorization decision. It rests
//! entirely on the caller identity the layer stamped: with enforcement disabled
//! every caller is `UNAUTHENTICATED_CALLER` and the check is vacuous (the H6
//! defect), so these tests run the harness with an **enforcing** layer whose
//! authenticator maps each client's token to a distinct identity name. That is the
//! only configuration in which "one workload cannot touch another's lease" is even
//! expressible over the wire, and it could not pass before this change.
//!
//! The standalone plugin backs the server, to stay hermetic (§7.6).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

use std::time::Duration;

use cluster_sdk::{ClusterClient, ClusterError, ElectionConfig, RemoteClusterClient};
use toolkit::contract_support::runtime::config::InternalTokenProvider;
use toolkit_security::authenticator::DynInternalAuthenticator;
use toolkit_security::internal_auth::{
    InternalAuthNError, InternalAuthenticator, PlatformIdentity,
};

mod common;
use common::served_gear::{PROFILE, ServedGear, served_gear};

/// The lock's TTL for every acquisition here: long enough that nothing lapses
/// during the test, so a failure is an authorization decision and never a timeout.
const TTL: Duration = Duration::from_secs(30);

/// An authenticator that resolves a token straight to an identity of the same
/// name, so a client dialling with token `"alice"` is the caller `alice`.
struct TokenIsName;

impl InternalAuthenticator for TokenIsName {
    async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
        Ok(PlatformIdentity::Shared {
            name: token.to_owned(),
        })
    }
}

/// A gear serving all four services behind an enforcing layer keyed by
/// [`TokenIsName`].
async fn enforcing_gear() -> ServedGear {
    served_gear()
        .authenticator(DynInternalAuthenticator::new(TokenIsName))
        .start()
        .await
}

/// A provider that attaches `token` as the platform-plane credential.
fn provider(token: &str) -> InternalTokenProvider {
    InternalTokenProvider::from_token(token.into())
}

/// A client that authenticates as the caller named `token`.
fn client_as(gear: &ServedGear, token: &str) -> RemoteClusterClient {
    gear.client_with(Some(&provider(token)))
}

#[tokio::test]
async fn a_caller_cannot_renew_another_callers_lock() {
    // The crisp form of the defect: `renew` predicates on `owns()`, so a caller
    // who is not the owner gets `LockExpired` — identical to a token matching
    // nothing, which is what keeps a live lease unprobeable (§6.9). With the
    // pre-fix vacuous check both callers are `UNAUTHENTICATED_CALLER`, so bob's
    // renew would SUCCEED.
    let gear = enforcing_gear().await;
    let alice = client_as(&gear, "alice");
    let bob = client_as(&gear, "bob");
    let alice_lock = alice.lock_backend(PROFILE).expect("a handle");
    let bob_lock = bob.lock_backend(PROFILE).expect("a handle");

    let token = alice_lock
        .acquire("ledger", "the-server-mints-it", TTL)
        .await
        .expect("alice acquires the lock");

    let err = bob_lock
        .renew(&token, TTL)
        .await
        .expect_err("bob does not own alice's lease");
    assert!(
        matches!(err, ClusterError::LockExpired { .. }),
        "a non-owner's renew must be LockExpired, got: {err}"
    );

    alice_lock
        .renew(&token, TTL)
        .await
        .expect("alice still owns her lease and can renew it");

    gear.stop().await;
}

#[tokio::test]
async fn a_caller_cannot_release_another_callers_lock() {
    // `release` predicates on `owns()` the other way: a non-owner's release is
    // `Ok` having done nothing (§12.6), so the lock stays held. Under the pre-fix
    // vacuous check bob's release would actually free alice's lock, and bob could
    // then take it.
    let gear = enforcing_gear().await;
    let alice = client_as(&gear, "alice");
    let bob = client_as(&gear, "bob");
    let alice_lock = alice.lock_backend(PROFILE).expect("a handle");
    let bob_lock = bob.lock_backend(PROFILE).expect("a handle");

    let token = alice_lock
        .acquire("gate", "the-server-mints-it", TTL)
        .await
        .expect("alice acquires the lock");

    bob_lock
        .release(&token)
        .await
        .expect("a non-owner's release is Ok, having done nothing");

    // The lock is still held by alice: bob cannot take it.
    let contended = bob_lock
        .try_lock("gate", TTL)
        .await
        .expect_err("alice's lock must still be held after bob's no-op release");
    assert!(
        matches!(contended, ClusterError::LockContended { .. }),
        "expected the lock still held (LockContended), got: {contended}"
    );

    // Alice's own release frees it, and then it is takeable.
    alice_lock
        .release(&token)
        .await
        .expect("the owner releases");
    let guard = bob_lock
        .try_lock("gate", TTL)
        .await
        .expect("the lock is free once its owner released it");
    guard.release().await.expect("release");

    gear.stop().await;
}

#[tokio::test]
async fn a_caller_cannot_renew_or_resign_another_callers_election() {
    // The leader primitive carries the same cross-check: `renew` is `LockExpired`
    // for a non-owner and `resign` is a silent no-op, so alice keeps her claim
    // across both of bob's attempts.
    let gear = enforcing_gear().await;
    let alice = client_as(&gear, "alice");
    let bob = client_as(&gear, "bob");
    let alice_leader = alice.leader_election_backend(PROFILE).expect("a handle");
    let bob_leader = bob.leader_election_backend(PROFILE).expect("a handle");
    let config = ElectionConfig::default();

    let token = alice_leader
        .join("primary", "the-server-mints-it", config)
        .await
        .expect("join")
        .expect("alice is the sole candidate and wins");

    let err = bob_leader
        .renew(&token, config.ttl())
        .await
        .expect_err("bob does not own alice's claim");
    assert!(
        matches!(err, ClusterError::LockExpired { .. }),
        "a non-owner's renew must be LockExpired, got: {err}"
    );

    bob_leader
        .resign(&token)
        .await
        .expect("a non-owner's resign is Ok, having done nothing");

    alice_leader
        .renew(&token, config.ttl())
        .await
        .expect("alice still holds her claim across bob's resign");

    gear.stop().await;
}

#[tokio::test]
async fn an_uncredentialed_call_is_rejected_before_the_handler() {
    // End-to-end enforcement: with the layer in `Required` mode, a client that
    // attaches no credential is refused by the layer before any service handler
    // runs. The shared `client()` carries no provider.
    let gear = enforcing_gear().await;
    let lock = gear.client().lock_backend(PROFILE).expect("a handle");

    let err = lock
        .acquire("ledger", "the-server-mints-it", TTL)
        .await
        .expect_err("an enforcing layer must refuse a credential-less call");
    assert!(
        err.to_string().to_lowercase().contains("unauthenticated"),
        "expected an Unauthenticated rejection from the layer, got: {err}"
    );

    gear.stop().await;
}
