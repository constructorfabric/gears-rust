// Created: 2026-06-11 by Constructor Tech
use async_trait::async_trait;

use super::ServiceDiscoveryBackend;
use crate::discovery::handle::ServiceHandle;
use crate::discovery::types::{
    DiscoveryFilter, InstanceState, ServiceDiscoveryFeatures, ServiceInstance, ServiceRegistration,
    StateFilter,
};
use crate::discovery::watch::ServiceWatch;
use crate::error::ClusterError;

/// A stub backend holding a single fixed instance; assigns `i-auto` when the
/// registration omits an id.
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
        filter: DiscoveryFilter,
    ) -> Result<Vec<ServiceInstance>, ClusterError> {
        let instance = ServiceInstance {
            instance_id: "i-1".to_owned(),
            address: "10.0.0.1:9000".to_owned(),
            metadata: std::collections::HashMap::new(),
            state: InstanceState::Enabled,
            registered_at: std::time::SystemTime::UNIX_EPOCH,
        };
        Ok(std::iter::once(instance)
            .filter(|i| filter.matches(i))
            .collect())
    }

    async fn watch(&self, _name: &str) -> Result<ServiceWatch, ClusterError> {
        let (_tx, watch) = ServiceWatch::channel(1);
        Ok(watch)
    }
}

// Auto-assignment of an absent instance id is a backend contract, so it is
// verified against the real `CacheBasedServiceDiscoveryBackend` in
// `defaults::discovery_tests` rather than against this stub (whose `register`
// hardcodes the id and would only test the stub itself).

#[tokio::test]
async fn discover_applies_the_filter() {
    let backend = StubBackend {
        metadata_pushdown: false,
    };
    // Default (enabled-only) matches the enabled stub instance.
    let Ok(found) = backend
        .discover("delivery", DiscoveryFilter::default())
        .await
    else {
        panic!("discover must succeed");
    };
    assert_eq!(found.len(), 1);
    // A disabled-only filter excludes it.
    let Ok(none) = backend
        .discover(
            "delivery",
            DiscoveryFilter::default().with_state(StateFilter::Disabled),
        )
        .await
    else {
        panic!("discover must succeed");
    };
    assert!(none.is_empty());
}

#[test]
fn provider_name_reports_concrete_backend() {
    // The consumer-visible contract: a capability mismatch error names the
    // concrete backend provider, not the dyn trait.
    use crate::discovery::resolver::validate_service_discovery_capabilities;
    use crate::discovery::types::ServiceDiscoveryCapability;
    let backend = StubBackend {
        metadata_pushdown: false,
    };
    let err = validate_service_discovery_capabilities(
        &backend,
        &[ServiceDiscoveryCapability::MetadataFiltering],
    )
    .unwrap_err();
    let ClusterError::CapabilityNotMet { provider, .. } = err else {
        panic!("expected CapabilityNotMet, got {err:?}");
    };
    assert!(
        provider.contains("StubBackend"),
        "provider field should name the concrete backend, got: {provider}"
    );
}
