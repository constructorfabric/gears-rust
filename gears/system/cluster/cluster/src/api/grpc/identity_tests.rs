//! Tests for caller resolution and the ownership cross-check.
//!
//! The cross-check is the one authorization decision `S1` owns (the backend's
//! lease methods are token-only), so it is tested as a decision rather than as a
//! string comparison: the questions asked here are "can one workload touch
//! another's lease" and "can one replica touch its sibling's".
//!
//! Resolution itself is now a read of the request extensions the platform-plane
//! `InternalAuthGrpcLayer` stamps (§4.6). These tests populate that extension
//! directly, which is exactly what the layer does over a real socket — the
//! `platform_credential` and `caller_ownership` integration tests prove the layer
//! actually does the stamping end-to-end.

use cluster_sdk::lease::LeaseToken;
use tonic::Request;
use toolkit_security::{PlatformIdentity, PlatformSecurityContext};

use super::{Caller, CallerResolver, UNAUTHENTICATED_CALLER};

fn caller_named(name: &str) -> Caller {
    Caller::new(PlatformSecurityContext::new(PlatformIdentity::Shared {
        name: name.to_owned(),
    }))
}

/// A request whose extensions carry `identity`, as the platform-plane layer
/// leaves them after a validated RPC.
fn request_stamped_with(identity: PlatformIdentity) -> Request<()> {
    let mut request = Request::new(());
    request
        .extensions_mut()
        .insert(PlatformSecurityContext::new(identity));
    request
}

#[test]
fn an_absent_identity_resolves_to_the_unauthenticated_caller() {
    // No extension: the layer stamped nothing (enforcement disabled, or a
    // permissive anonymous call). The caller falls back rather than being
    // rejected — the honest consequence of serving without enforcement.
    let caller =
        CallerResolver::resolve(&Request::new(())).expect("an absent identity is not an error");
    assert_eq!(caller.name(), UNAUTHENTICATED_CALLER);
}

#[test]
fn a_stamped_identity_names_the_caller() {
    // The layer validated the credential and stamped the context; the resolver
    // reports the ServiceAccount name as the ClientId (§4.6).
    let caller = CallerResolver::resolve(&request_stamped_with(
        PlatformIdentity::KubernetesServiceAccount {
            namespace: "toolkit".to_owned(),
            service_account: "event-broker".to_owned(),
            pod: Some("event-broker-0".to_owned()),
        },
    ))
    .expect("a stamped identity resolves");
    assert_eq!(
        caller.name(),
        "event-broker",
        "the ServiceAccount name is the ClientId (DESIGN section 4.6)"
    );
}

#[test]
fn distinct_stamped_identities_yield_distinct_callers() {
    // The property `owns()` rests on: two different stamped identities are two
    // different callers, so the layer is what makes the cross-check meaningful.
    let broker = CallerResolver::resolve(&request_stamped_with(PlatformIdentity::Shared {
        name: "event-broker".to_owned(),
    }))
    .expect("resolves");
    let gateway = CallerResolver::resolve(&request_stamped_with(PlatformIdentity::Shared {
        name: "api-gateway".to_owned(),
    }))
    .expect("resolves");
    assert_ne!(broker.name(), gateway.name());
}

#[test]
fn an_owner_carries_the_caller_and_a_fresh_nonce() {
    let caller = caller_named("event-broker");
    let first = caller.mint_owner();
    let second = caller.mint_owner();

    assert!(first.starts_with("event-broker/"));
    assert_ne!(
        first, second,
        "two acquisitions by one caller must not share an owner, or releasing one \
         would match the other's record"
    );
}

#[test]
fn a_caller_owns_only_the_tokens_minted_for_it() {
    let broker = caller_named("event-broker");
    let gateway = caller_named("api-gateway");

    let token = LeaseToken::new("ledger", broker.mint_owner(), 3);
    assert!(broker.owns(&token));
    assert!(
        !gateway.owns(&token),
        "one workload must not be able to renew or release another's lease"
    );
}

#[test]
fn two_replicas_of_one_workload_are_distinct_holders() {
    // Both resolve to the same ClientId - they run under one ServiceAccount - so
    // the nonce is the only thing separating their leases. Without it, `fence`
    // counts from 1 and a lock name is often well known, so one replica could
    // forge its sibling's token by guessing a small integer. It would also make a
    // lock *between* the two replicas unrepresentable.
    let replica_a = caller_named("event-broker");
    let replica_b = caller_named("event-broker");

    let token_a = LeaseToken::new("ledger", replica_a.mint_owner(), 1);
    let token_b = LeaseToken::new("ledger", replica_b.mint_owner(), 2);
    assert_ne!(token_a.owner, token_b.owner);
}

#[test]
fn a_token_with_no_nonce_belongs_to_nobody() {
    // An in-process holder marker is a bare UUID, and a fabricated token is
    // whatever its author wrote. Neither was minted by this service, so neither
    // is any caller's.
    let caller = caller_named("event-broker");
    assert!(!caller.owns(&LeaseToken::new("ledger", "event-broker", 1)));
    assert!(!caller.owns(&LeaseToken::new("ledger", "", 0)));
    assert!(!caller.owns(&LeaseToken::new(
        "ledger",
        "3f2504e0-4f89-11d3-9a0c-0305e82c3301",
        1
    )));
}
