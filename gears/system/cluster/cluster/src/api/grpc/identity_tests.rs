//! Tests for caller resolution and the ownership cross-check.
//!
//! The cross-check is the one authorization decision `S1` owns (the backend's
//! lease methods are token-only), so it is tested as a decision rather than as a
//! string comparison: the questions asked here are "can one workload touch
//! another's lease" and "can one replica touch its sibling's".

use cluster_sdk::lease::LeaseToken;
use tonic::metadata::{MetadataMap, MetadataValue};
use toolkit_security::constants::INTERNAL_TOKEN_HEADER;
use toolkit_security::{PlatformIdentity, PlatformSecurityContext};

use super::{Caller, CallerAuthentication, CallerResolver, UNAUTHENTICATED_CALLER};

fn caller_named(name: &str) -> Caller {
    Caller::new(PlatformSecurityContext::new(PlatformIdentity::Shared {
        name: name.to_owned(),
    }))
}

/// Metadata carrying a platform-plane credential, as an inbound call has it.
fn with_internal_token(token: &str) -> MetadataMap {
    let mut metadata = MetadataMap::new();
    metadata.insert(
        INTERNAL_TOKEN_HEADER,
        MetadataValue::try_from(token).expect("an ASCII token"),
    );
    metadata
}

#[tokio::test]
async fn the_credential_is_read_from_the_platform_plane_header() {
    // The exit criterion, asserted at the only place it can be: the resolver
    // accepts a call carrying `x-toolkit-internal-token` and nothing else. If it
    // were reading `x-secctx-bin` this call would carry no credential at all.
    let resolver = CallerResolver::trusted_network();
    let caller = resolver
        .resolve(&with_internal_token("sa-token"))
        .await
        .expect("a credential on the platform-plane header resolves");
    assert_eq!(caller.name(), UNAUTHENTICATED_CALLER);
}

#[tokio::test]
async fn a_tenant_plane_secctx_header_is_not_a_credential() {
    // `x-secctx-bin` is scoped to in-process gRPC metadata in Profile 1, and
    // ADR-0008 drops it from the cross-process contract (DESIGN section 4.6). A
    // call carrying only that header must be treated as carrying nothing - which
    // in a validated deployment means rejected.
    let mut metadata = MetadataMap::new();
    metadata.insert_bin(
        toolkit_transport_grpc::SECCTX_METADATA_KEY,
        MetadataValue::from_bytes(b"a tenant context"),
    );

    let status = validating_resolver()
        .resolve(&metadata)
        .await
        .expect_err("a tenant-plane header is not a platform-plane credential");
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

/// A resolver that validates, accepting exactly one token.
fn validating_resolver() -> CallerResolver {
    use toolkit_security::{DynInternalAuthenticator, InternalAuthNError, InternalAuthenticator};

    struct OnlyGoodTokens;

    impl InternalAuthenticator for OnlyGoodTokens {
        async fn authenticate(&self, token: &str) -> Result<PlatformIdentity, InternalAuthNError> {
            match token {
                "event-broker-token" => Ok(PlatformIdentity::KubernetesServiceAccount {
                    namespace: "toolkit".to_owned(),
                    service_account: "event-broker".to_owned(),
                    pod: Some("event-broker-0".to_owned()),
                }),
                "backend-down" => Err(InternalAuthNError::Unavailable),
                _ => Err(InternalAuthNError::InvalidToken),
            }
        }
    }

    CallerResolver::validated(DynInternalAuthenticator::new(OnlyGoodTokens))
}

#[tokio::test]
async fn a_validated_credential_names_the_caller() {
    let caller = validating_resolver()
        .resolve(&with_internal_token("event-broker-token"))
        .await
        .expect("a good credential resolves");
    assert_eq!(
        caller.name(),
        "event-broker",
        "the ServiceAccount name is the ClientId (DESIGN section 4.6)"
    );
}

#[tokio::test]
async fn a_rejected_credential_is_unauthenticated_and_a_missing_one_too() {
    let resolver = validating_resolver();

    let rejected = resolver
        .resolve(&with_internal_token("forged"))
        .await
        .expect_err("a bad credential is refused");
    assert_eq!(rejected.code(), tonic::Code::Unauthenticated);

    let absent = resolver
        .resolve(&MetadataMap::new())
        .await
        .expect_err("no credential is refused");
    assert_eq!(absent.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn an_unreachable_validator_is_unavailable_not_unauthenticated() {
    // The caller's credential may be perfectly good; it is cluster's own
    // dependency that is down. `Unauthenticated` would send the platform's
    // internal-auth middleware down a token-refresh path that cannot help, which
    // is the same mistake `Provider{AuthFailure}` -> `Internal` avoids
    // (DESIGN section 6.9).
    let status = validating_resolver()
        .resolve(&with_internal_token("backend-down"))
        .await
        .expect_err("an unreachable validator fails the call");
    assert_eq!(status.code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn the_trusted_network_mode_accepts_a_call_with_no_credential() {
    // v1 ships with nothing validating the credential, so requiring it would
    // reject the honest caller and admit the dishonest one. The mode is named so
    // this is a visible choice rather than an inferred fallback.
    let caller = CallerResolver::trusted_network()
        .resolve(&MetadataMap::new())
        .await
        .expect("no inbound authenticator means no rejection");
    assert_eq!(caller.name(), UNAUTHENTICATED_CALLER);
    assert!(matches!(
        CallerResolver::trusted_network().mode(),
        CallerAuthentication::TrustedNetwork
    ));
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
