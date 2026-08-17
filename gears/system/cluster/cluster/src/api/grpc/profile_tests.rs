//! Tests for the profile/discovery service.

use cluster_sdk::grpc::stubs::profile as stubs;
use cluster_sdk::grpc::stubs::profile::cluster_profile_api_server::ClusterProfileApi as _;

use super::super::test_harness::{Harness, request};
use super::ClusterProfileService;

fn describe(profiles: &[&str]) -> stubs::DescribeProfilesRequest {
    stubs::DescribeProfilesRequest {
        profiles: profiles.iter().map(|name| (*name).to_owned()).collect(),
    }
}

#[tokio::test]
async fn an_empty_request_describes_every_profile_in_name_order() {
    // Deterministic across replicas and across calls, because the snapshot is a
    // `BTreeMap` - which is what lets a client compare two responses at all.
    let harness = Harness::wired(&["orders", "audit", "event-broker"]).await;
    let service = ClusterProfileService::new(harness.ctx.clone());

    let response = service
        .describe_profiles(request(describe(&[])))
        .await
        .expect("describe succeeds")
        .into_inner();

    let names: Vec<&str> = response
        .profiles
        .iter()
        .map(|profile| profile.name.as_str())
        .collect();
    assert_eq!(names, vec!["audit", "event-broker", "orders"]);
    assert_eq!(response.generation, harness.registry.generation());

    harness.stop().await;
}

#[tokio::test]
async fn a_descriptor_reports_the_real_backend_not_the_transport() {
    // The provider a consumer is told about is the *server-side* one, never
    // "remote": when a capability requirement fails, the operator has to see
    // which real backend failed it (DESIGN section 5.5).
    let harness = Harness::wired(&["orders"]).await;
    let service = ClusterProfileService::new(harness.ctx.clone());

    let response = service
        .describe_profiles(request(describe(&["orders"])))
        .await
        .expect("describe succeeds")
        .into_inner();

    let profile = response.profiles.first().expect("one profile");
    let cache = profile.cache.as_ref().expect("a cache descriptor");
    assert_eq!(cache.provider, "standalone");
    assert_ne!(cache.provider, "remote");
    assert!(
        profile.lock.is_some(),
        "an omitted primitive still describes"
    );
    assert!(profile.leader_election.is_some());

    harness.stop().await;
}

#[tokio::test]
async fn a_named_subset_is_returned_in_the_order_asked_for() {
    let harness = Harness::wired(&["orders", "audit"]).await;
    let service = ClusterProfileService::new(harness.ctx.clone());

    let response = service
        .describe_profiles(request(describe(&["orders", "audit"])))
        .await
        .expect("describe succeeds")
        .into_inner();

    let names: Vec<&str> = response
        .profiles
        .iter()
        .map(|profile| profile.name.as_str())
        .collect();
    assert_eq!(names, vec!["orders", "audit"]);

    harness.stop().await;
}

#[tokio::test]
async fn a_named_profile_that_is_not_bound_is_an_error_not_an_omission() {
    // An empty request already means "all", so dropping an unknown name would be
    // indistinguishable from a deployment that binds nothing - and the consumer
    // asking is about to gate its own readiness on the answer (section 4.7.1).
    let harness = Harness::wired(&["orders"]).await;
    let service = ClusterProfileService::new(harness.ctx.clone());

    let status = service
        .describe_profiles(request(describe(&["orders", "not-a-profile"])))
        .await
        .expect_err("an unbound name is refused");
    assert_eq!(status.code(), tonic::Code::NotFound);

    harness.stop().await;
}

#[tokio::test]
async fn describing_before_start_publishes_reports_an_empty_set_at_generation_zero() {
    // The `init` -> `start` window again. An empty *unnamed* request is not an
    // error here: "nothing is bound yet" is the true answer, and it is what a
    // consumer's readiness contributor (item `K5`) acts on.
    let harness = Harness::unpublished().await;
    let service = ClusterProfileService::new(harness.ctx.clone());

    let response = service
        .describe_profiles(request(describe(&[])))
        .await
        .expect("describe succeeds")
        .into_inner();
    assert!(response.profiles.is_empty());
    assert_eq!(response.generation, 0);

    // Naming a profile in that window is still `ProfileNotBound`.
    let status = service
        .describe_profiles(request(describe(&["orders"])))
        .await
        .expect_err("nothing is bound yet");
    assert_eq!(status.code(), tonic::Code::NotFound);

    harness.stop().await;
}
