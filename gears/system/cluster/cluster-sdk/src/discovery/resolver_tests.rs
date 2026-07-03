// Created: 2026-06-11 by Constructor Tech
use std::sync::Arc;

use async_trait::async_trait;
use toolkit::client_hub::ClientHub;

use super::{ServiceDiscoveryResolverBuilder, validate_service_discovery_capabilities};
use crate::discovery::backend::ServiceDiscoveryBackend;
use crate::discovery::facade::ServiceDiscoveryV1;
use crate::discovery::handle::ServiceHandle;
use crate::discovery::types::{
    DiscoveryFilter, ServiceDiscoveryCapability, ServiceDiscoveryFeatures, ServiceInstance,
    ServiceRegistration,
};
use crate::discovery::watch::ServiceWatch;
use crate::error::ClusterError;
use crate::profile::{ClusterProfile, profile_scope};

struct StubBackend {
    metadata_pushdown: bool,
}

#[async_trait]
impl ServiceDiscoveryBackend for StubBackend {
    fn features(&self) -> ServiceDiscoveryFeatures {
        ServiceDiscoveryFeatures::new(self.metadata_pushdown)
    }
    async fn register(&self, reg: ServiceRegistration) -> Result<ServiceHandle, ClusterError> {
        let id = reg.instance_id.unwrap_or_else(|| "i-auto".to_owned());
        let (_rx, handle) = ServiceHandle::channel(id, 1);
        Ok(handle)
    }
    async fn discover(
        &self,
        _name: &str,
        _filter: DiscoveryFilter,
    ) -> Result<Vec<ServiceInstance>, ClusterError> {
        Ok(Vec::new())
    }
    async fn watch(&self, _name: &str) -> Result<ServiceWatch, ClusterError> {
        let (_tx, watch) = ServiceWatch::channel(1);
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
    let backend = StubBackend {
        metadata_pushdown: true,
    };
    assert!(
        validate_service_discovery_capabilities(
            &backend,
            &[ServiceDiscoveryCapability::MetadataFiltering]
        )
        .is_ok()
    );
}

#[test]
fn validate_rejects_unmet_metadata_filtering() {
    let backend = StubBackend {
        metadata_pushdown: false,
    };
    let Err(ClusterError::CapabilityNotMet {
        primitive,
        capability,
        provider,
    }) = validate_service_discovery_capabilities(
        &backend,
        &[ServiceDiscoveryCapability::MetadataFiltering],
    )
    else {
        panic!("an unmet metadata-filtering requirement must be rejected");
    };
    assert_eq!(primitive, "ServiceDiscoveryV1");
    assert_eq!(capability, "MetadataFiltering");
    // The error names the concrete backend, not the erased `dyn` trait type.
    assert!(
        provider.contains("StubBackend"),
        "provider should name the concrete backend, got `{provider}`"
    );
}

#[test]
fn resolve_without_profile_errors() {
    let hub = ClientHub::new();
    let result = ServiceDiscoveryResolverBuilder::new(&hub).resolve();
    assert!(matches!(result, Err(ClusterError::ProfileNotSpecified)));
}

#[test]
fn resolve_unbound_profile_errors() {
    let hub = ClientHub::new();
    let result = ServiceDiscoveryV1::resolver(&hub)
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
    let backend: Arc<dyn ServiceDiscoveryBackend> = Arc::new(StubBackend {
        metadata_pushdown: true,
    });
    hub.register_scoped::<dyn ServiceDiscoveryBackend>(scope, backend);

    let Ok(sd) = ServiceDiscoveryV1::resolver(&hub)
        .profile(EventBrokerProfile)
        .require(ServiceDiscoveryCapability::MetadataFiltering)
        .resolve()
    else {
        panic!("resolution against a matching backend must succeed");
    };
    assert!(sd.features().metadata_pushdown);
}

#[test]
fn resolve_rejects_capability_mismatch_at_startup() {
    let hub = ClientHub::new();
    let Ok(scope) = profile_scope(EventBrokerProfile::NAME) else {
        panic!("valid profile name must produce a scope");
    };
    let backend: Arc<dyn ServiceDiscoveryBackend> = Arc::new(StubBackend {
        metadata_pushdown: false,
    });
    hub.register_scoped::<dyn ServiceDiscoveryBackend>(scope, backend);

    let result = ServiceDiscoveryV1::resolver(&hub)
        .profile(EventBrokerProfile)
        .require(ServiceDiscoveryCapability::MetadataFiltering)
        .resolve();
    assert!(matches!(
        result,
        Err(ClusterError::CapabilityNotMet {
            capability: "MetadataFiltering",
            ..
        })
    ));
}
