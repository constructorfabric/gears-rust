// Created: 2026-06-11 by Constructor Tech
use std::sync::Arc;

use async_trait::async_trait;
use toolkit::client_hub::ClientHub;

use super::{LeaderElectionResolverBuilder, validate_leader_election_capabilities};
use crate::error::ClusterError;
use crate::leader::backend::LeaderElectionBackend;
use crate::leader::facade::LeaderElectionV1;
use crate::leader::types::{
    ElectionConfig, LeaderElectionCapability, LeaderElectionFeatures, LeaderStatus,
};
use crate::leader::watch::LeaderWatch;
use crate::profile::ClusterProfile;
use crate::test_support::StubClusterClient;
use crate::test_support::with_nothing_derivable;

struct StubBackend {
    linearizable: bool,
}

#[async_trait]
impl LeaderElectionBackend for StubBackend {
    fn features(&self) -> LeaderElectionFeatures {
        LeaderElectionFeatures::new(self.linearizable)
    }
    async fn elect(&self, _name: &str) -> Result<LeaderWatch, ClusterError> {
        let (_tx, _resign, watch) = LeaderWatch::channel(1, LeaderStatus::Follower);
        Ok(watch)
    }
    async fn elect_with_config(
        &self,
        _name: &str,
        _config: ElectionConfig,
    ) -> Result<LeaderWatch, ClusterError> {
        let (_tx, _resign, watch) = LeaderWatch::channel(1, LeaderStatus::Follower);
        Ok(watch)
    }
}

#[derive(Clone, Copy)]
struct EventBrokerProfile;
impl ClusterProfile for EventBrokerProfile {
    const NAME: &'static str = "event-broker";
}

#[test]
fn validate_passes_when_capability_met() {
    let backend = StubBackend { linearizable: true };
    assert!(
        validate_leader_election_capabilities(&backend, &[LeaderElectionCapability::Linearizable])
            .is_ok()
    );
}

#[test]
fn validate_rejects_unmet_linearizable() {
    let backend = StubBackend {
        linearizable: false,
    };
    let Err(ClusterError::CapabilityNotMet {
        capability,
        provider,
        ..
    }) = validate_leader_election_capabilities(&backend, &[LeaderElectionCapability::Linearizable])
    else {
        panic!("an unmet linearizable requirement must be rejected");
    };
    assert_eq!(capability, "Linearizable");
    // The error names the concrete backend, not the erased `dyn` trait type.
    assert!(
        provider.contains("StubBackend"),
        "provider should name the concrete backend, got `{provider}`"
    );
}

#[tokio::test]
async fn resolve_without_profile_errors() {
    let hub = ClientHub::new();
    let result = LeaderElectionResolverBuilder::new(&hub).resolve().await;
    assert!(matches!(result, Err(ClusterError::ProfileNotSpecified)));
}

/// Registers a cluster client binding `backend` to the event-broker profile —
/// since `K4` the resolvers bind through `dyn ClusterClient` rather than through a
/// per-profile hub registration (DESIGN-DEPLOYABLE-GEAR §4.9.3).
fn client_with(hub: &ClientHub, backend: StubBackend) {
    StubClusterClient::for_profile(EventBrokerProfile::NAME)
        .with_leader_election(Arc::new(backend))
        .register(hub);
}

#[tokio::test]
async fn resolve_unbound_profile_errors() {
    let hub = ClientHub::new();
    // Cluster is reachable and binds a different profile: a permanent config
    // error, returned by `resolve()` itself (DESIGN §4.7).
    StubClusterClient::for_profile("other")
        .with_leader_election(Arc::new(StubBackend { linearizable: true }))
        .register(&hub);

    let result = LeaderElectionV1::resolver(&hub)
        .profile(EventBrokerProfile)
        .resolve()
        .await;
    assert!(matches!(
        result,
        Err(ClusterError::ProfileNotBound {
            profile: "event-broker"
        })
    ));
}

/// Nothing wired: `resolve()` succeeds and the first call reports it (§4.9.1).
#[tokio::test]
async fn resolve_with_nothing_wired_succeeds_and_the_first_call_reports_it() {
    let hub = ClientHub::new();

    // See `with_nothing_derivable`: without it, a concurrent test's `POD_NAMESPACE`
    // would let the resolve path self-construct a client and bind this facade.
    let Ok(leader) = with_nothing_derivable(
        LeaderElectionV1::resolver(&hub)
            .profile(EventBrokerProfile)
            .resolve(),
    )
    .await
    else {
        panic!("an empty hub must not fail resolution");
    };

    assert!(!leader.features().linearizable);
    assert!(matches!(
        leader.elect("primary").await,
        Err(ClusterError::ProfileNotBound {
            profile: "event-broker"
        })
    ));
}

#[tokio::test]
async fn resolve_happy_path_returns_facade() {
    let hub = ClientHub::new();
    client_with(&hub, StubBackend { linearizable: true });

    let Ok(leader) = LeaderElectionV1::resolver(&hub)
        .profile(EventBrokerProfile)
        .require(LeaderElectionCapability::Linearizable)
        .resolve()
        .await
    else {
        panic!("resolution against a matching backend must succeed");
    };
    assert!(leader.features().linearizable);
}

#[tokio::test]
async fn resolve_rejects_capability_mismatch_at_startup() {
    let hub = ClientHub::new();
    client_with(
        &hub,
        StubBackend {
            linearizable: false,
        },
    );

    let result = LeaderElectionV1::resolver(&hub)
        .profile(EventBrokerProfile)
        .require(LeaderElectionCapability::Linearizable)
        .resolve()
        .await;
    assert!(matches!(
        result,
        Err(ClusterError::CapabilityNotMet {
            capability: "Linearizable",
            ..
        })
    ));
}
