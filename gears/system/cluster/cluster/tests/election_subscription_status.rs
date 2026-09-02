//! The status an election subscription raises, decoded with the lease context
//! that names it.
//!
//! `AwaitChange` addresses a *subscription*, which is replica-local, so the
//! server answers a bare `NotFound` whenever the replica serving it went away
//! (`api/grpc/leader.rs`, `Status::not_found("unknown election_id")`). Section
//! 6.9's table maps that row to `Closed(ClusterError::Shutdown)` — terminal, so
//! `RestartingWatch` propagates rather than resubscribing.
//!
//! The premise has to be checked against a real server rather than a hand-built
//! `Status`: the server's answer carries **no** cluster error envelope, so the
//! decode depends entirely on the `LeaseContext` the caller passes.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "integration tests: a setup failure IS the test failure"
)]

use cluster_sdk::grpc::stubs;
use cluster_sdk::{ClusterError, LeaseContext, from_lease_status, from_status};

mod common;
use common::served_gear::{PROFILE, Services, served_gear};

/// Asks a real gear to `await_change` on an election it never issued, and hands
/// back the `Status` it answered with.
async fn unknown_subscription_status() -> tonic::Status {
    // Leader-only: the premise is about the status *this* service raises, so
    // nothing else needs serving.
    let gear = served_gear().services(Services::LEADER).start().await;

    let mut raw = stubs::leader::leader_election_api_client::LeaderElectionApiClient::connect(
        gear.endpoint.clone(),
    )
    .await
    .expect("connects");

    let status = raw
        .await_change(stubs::leader::AwaitChangeRequest {
            profile: PROFILE.to_owned(),
            election_id: "an-election-this-replica-never-issued".to_owned(),
        })
        .await
        .expect_err("an unknown election_id must not open a stream");

    gear.stop().await;
    status
}

/// End to end: the same real status decodes two different ways, and only one of
/// them is the answer section 6.9 specifies.
#[tokio::test]
async fn a_subscription_notfound_decodes_as_shutdown_only_with_its_lease_context() {
    let status = unknown_subscription_status().await;

    // The premise: a bare `NotFound`, with no cluster error envelope for the
    // codec to key on. If that ever stops holding, the rest of this test is
    // asserting nothing, and this is what says so.
    assert_eq!(status.code(), tonic::Code::NotFound, "{status:?}");
    assert!(
        status.metadata().get_bin("x-toolkit-problem-bin").is_none(),
        "the server types no cluster error here - that is what makes the LeaseContext \
         load-bearing"
    );

    // Decoded without the context — `from_status`, i.e. `LeaseContext::None`.
    let without_context = from_status(&status);
    assert!(
        matches!(
            without_context,
            ClusterError::Provider {
                kind: cluster_sdk::ProviderErrorKind::Other,
                ..
            }
        ),
        "without a lease context the codec has nothing to key on, got {without_context:?}"
    );

    // What section 6.9's table specifies.
    let with_context = from_lease_status(&status, LeaseContext::ElectionSubscription)
        .expect("a subscription absence is an error, not release-by-absence");
    assert!(
        matches!(with_context, ClusterError::Shutdown),
        "a subscription whose replica went away is a terminal shutdown, got {with_context:?}"
    );

    // Retryability agrees either way - so this is minor, and why the
    // consumer-visible variant is the whole of the difference.
    assert!(!without_context.is_retryable());
    assert!(!with_context.is_retryable());
}
