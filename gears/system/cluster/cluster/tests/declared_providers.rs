//! The four providers `ClusterGear` declares with `#[toolkit::provides(transports
//! = [grpc])]` are answered by a real served gear through the gRPC clients the
//! declarations name. This is the
//! client a composition tool binds a remote requester to; the SDK's
//! `RemoteClusterClient` reaches the same services through the same stubs.
//! Each test drives its contract's lifecycle (acquire, renew, release) rather
//! than a single call, so a provider that only connects does not pass.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use cluster_sdk::contract::{
    ClusterCacheApi, ClusterProfileApi, DistributedLockApi, LeaderElectionApi,
};
use cluster_sdk::dto::{
    DescribeProfilesRequest, GetRequest, JoinRequest, LeaseRef, TryLockRequest, WireLeaderStatus,
};
use cluster_sdk::grpc::{
    ClusterCacheApiGrpcClient, ClusterProfileApiGrpcClient, DistributedLockApiGrpcClient,
    LeaderElectionApiGrpcClient,
};
use toolkit_canonical_errors::CanonicalError;
use toolkit_contract::runtime::config::ClientConfig;
use toolkit_security::PlatformSecurityContext;

mod common;
use common::served_gear::served_gear;

#[tokio::test]
async fn declared_profile_provider_answers_through_its_grpc_client() {
    let served = served_gear().start().await;
    let client = ClusterProfileApiGrpcClient::connect(ClientConfig::new(served.endpoint.clone()))
        .await
        .expect("connect");

    let response = client
        .describe_profiles(
            &PlatformSecurityContext::outbound_marker(),
            DescribeProfilesRequest { profiles: vec![] },
        )
        .await
        .expect("describe_profiles");

    assert!(
        !response.profiles.is_empty(),
        "the served gear binds at least one profile"
    );
    served.stop().await;
}

#[tokio::test]
async fn declared_cache_provider_answers_through_its_grpc_client() {
    let served = served_gear().start().await;
    let profiles = ClusterProfileApiGrpcClient::connect(ClientConfig::new(served.endpoint.clone()))
        .await
        .expect("connect")
        .describe_profiles(
            &PlatformSecurityContext::outbound_marker(),
            DescribeProfilesRequest { profiles: vec![] },
        )
        .await
        .expect("describe_profiles");
    let profile = profiles.profiles[0].name.clone();

    let cache = ClusterCacheApiGrpcClient::connect(ClientConfig::new(served.endpoint.clone()))
        .await
        .expect("connect");
    let missing = cache
        .get(
            &PlatformSecurityContext::outbound_marker(),
            GetRequest {
                profile,
                key: "declared-providers/absent".to_owned(),
            },
        )
        .await
        .expect("a missing key is Ok, never an error");
    assert!(missing.entry.is_none());
    served.stop().await;
}

async fn first_profile(endpoint: &str) -> String {
    ClusterProfileApiGrpcClient::connect(ClientConfig::new(endpoint.to_owned()))
        .await
        .expect("connect")
        .describe_profiles(
            &PlatformSecurityContext::outbound_marker(),
            DescribeProfilesRequest { profiles: vec![] },
        )
        .await
        .expect("describe_profiles")
        .profiles[0]
        .name
        .clone()
}

#[tokio::test]
async fn declared_lock_provider_answers_through_its_grpc_client() {
    let served = served_gear().start().await;
    let profile = first_profile(&served.endpoint).await;
    let ctx = PlatformSecurityContext::outbound_marker();
    let lock = DistributedLockApiGrpcClient::connect(ClientConfig::new(served.endpoint.clone()))
        .await
        .expect("connect");
    let request = || TryLockRequest {
        profile: profile.clone(),
        name: "declared-providers/lock".to_owned(),
        ttl_ms: 30_000,
        client_request_id: None,
    };

    let held = lock.try_lock(&ctx, request()).await.expect("try_lock");
    let contended = lock
        .try_lock(&ctx, request())
        .await
        .expect_err("a held lock is contended");
    assert!(
        matches!(contended, CanonicalError::AlreadyExists { .. }),
        "contention keeps its category over the wire, got {contended:?}"
    );
    let lease = LeaseRef {
        profile: profile.clone(),
        token: held.token,
        ttl_ms: Some(30_000),
        client_request_id: None,
    };
    lock.renew(&ctx, lease.clone()).await.expect("renew");
    lock.release(&ctx, lease).await.expect("release");
    lock.try_lock(&ctx, request())
        .await
        .expect("a released lock can be taken again");
    served.stop().await;
}

#[tokio::test]
async fn declared_leader_election_provider_answers_through_its_grpc_client() {
    let served = served_gear().start().await;
    let profile = first_profile(&served.endpoint).await;
    let ctx = PlatformSecurityContext::outbound_marker();
    let election = LeaderElectionApiGrpcClient::connect(ClientConfig::new(served.endpoint.clone()))
        .await
        .expect("connect");

    let joined = election
        .join(
            &ctx,
            JoinRequest {
                profile: profile.clone(),
                name: "declared-providers/election".to_owned(),
                ttl_ms: 30_000,
                max_missed_renewals: None,
                client_request_id: None,
            },
        )
        .await
        .expect("join");
    assert_eq!(
        joined.initial_status,
        WireLeaderStatus::Leader,
        "the only candidate leads"
    );
    let lease = LeaseRef {
        profile,
        token: joined.token,
        ttl_ms: Some(30_000),
        client_request_id: None,
    };
    election.renew(&ctx, lease.clone()).await.expect("renew");
    election.resign(&ctx, lease).await.expect("resign");
    served.stop().await;
}
