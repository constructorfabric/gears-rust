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
use crate::profile::{ClusterProfile, profile_scope};

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

#[test]
fn resolve_without_profile_errors() {
    let hub = ClientHub::new();
    let result = LeaderElectionResolverBuilder::new(&hub).resolve();
    assert!(matches!(result, Err(ClusterError::ProfileNotSpecified)));
}

#[test]
fn resolve_unbound_profile_errors() {
    let hub = ClientHub::new();
    let result = LeaderElectionV1::resolver(&hub)
        .profile(EventBrokerProfile)
        .resolve();
    assert!(matches!(
        result,
        Err(ClusterError::ProfileNotBound {
            profile: "event-broker"
        })
    ));
}

#[test]
fn resolve_happy_path_returns_facade() {
    let hub = ClientHub::new();
    let Ok(scope) = profile_scope(EventBrokerProfile::NAME) else {
        panic!("valid profile name must produce a scope");
    };
    let backend: Arc<dyn LeaderElectionBackend> = Arc::new(StubBackend { linearizable: true });
    hub.register_scoped::<dyn LeaderElectionBackend>(scope, backend);

    let Ok(leader) = LeaderElectionV1::resolver(&hub)
        .profile(EventBrokerProfile)
        .require(LeaderElectionCapability::Linearizable)
        .resolve()
    else {
        panic!("resolution against a matching backend must succeed");
    };
    assert!(leader.features().linearizable);
}

#[test]
fn resolve_rejects_capability_mismatch_at_startup() {
    let hub = ClientHub::new();
    let Ok(scope) = profile_scope(EventBrokerProfile::NAME) else {
        panic!("valid profile name must produce a scope");
    };
    let backend: Arc<dyn LeaderElectionBackend> = Arc::new(StubBackend {
        linearizable: false,
    });
    hub.register_scoped::<dyn LeaderElectionBackend>(scope, backend);

    let result = LeaderElectionV1::resolver(&hub)
        .profile(EventBrokerProfile)
        .require(LeaderElectionCapability::Linearizable)
        .resolve();
    assert!(matches!(
        result,
        Err(ClusterError::CapabilityNotMet {
            capability: "Linearizable",
            ..
        })
    ));
}
