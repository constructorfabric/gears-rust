// Created: 2026-06-11 by Constructor Tech
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::ScopedServiceDiscoveryBackend;
use crate::discovery::backend::ServiceDiscoveryBackend;
use crate::discovery::handle::ServiceHandle;
use crate::discovery::types::{
    DiscoveryFilter, ServiceDiscoveryFeatures, ServiceInstance, ServiceRegistration,
};
use crate::discovery::watch::ServiceWatch;
use crate::error::ClusterError;
use crate::scope;

/// Records the service name and the registration's metadata the backend saw.
struct RecordingBackend {
    names: Mutex<Vec<String>>,
    last_metadata: Mutex<HashMap<String, String>>,
}

#[async_trait]
impl ServiceDiscoveryBackend for RecordingBackend {
    fn features(&self) -> ServiceDiscoveryFeatures {
        ServiceDiscoveryFeatures::new(false)
    }

    async fn register(&self, reg: ServiceRegistration) -> Result<ServiceHandle, ClusterError> {
        self.names.lock().expect("lock").push(reg.name.clone());
        *self.last_metadata.lock().expect("lock") = reg.metadata;
        let (_rx, handle) = ServiceHandle::channel("i-1".to_owned(), 1);
        Ok(handle)
    }

    async fn discover(
        &self,
        name: &str,
        _filter: DiscoveryFilter,
    ) -> Result<Vec<ServiceInstance>, ClusterError> {
        self.names.lock().expect("lock").push(name.to_owned());
        Ok(Vec::new())
    }

    async fn watch(&self, name: &str) -> Result<ServiceWatch, ClusterError> {
        self.names.lock().expect("lock").push(name.to_owned());
        let (_tx, watch) = ServiceWatch::channel(1);
        Ok(watch)
    }
}

fn scoped(inner: Arc<RecordingBackend>, prefix: &str) -> ScopedServiceDiscoveryBackend {
    ScopedServiceDiscoveryBackend::new(
        inner,
        scope::validated_prefix(prefix).expect("valid prefix"),
    )
}

#[tokio::test]
async fn register_scopes_name_but_not_metadata() {
    let backend = Arc::new(RecordingBackend {
        names: Mutex::new(Vec::new()),
        last_metadata: Mutex::new(HashMap::new()),
    });
    let wrapper = scoped(Arc::clone(&backend), "event-broker");
    let mut metadata = HashMap::new();
    metadata.insert("region".to_owned(), "us-east".to_owned());
    assert!(
        wrapper
            .register(ServiceRegistration {
                name: "delivery".to_owned(),
                instance_id: None,
                address: "10.0.0.1:9000".to_owned(),
                metadata: metadata.clone(),
            })
            .await
            .is_ok()
    );
    assert_eq!(
        backend.names.lock().expect("lock").as_slice(),
        ["event-broker/delivery"]
    );
    // Metadata is a per-instance attribute namespace — never scoped.
    assert_eq!(*backend.last_metadata.lock().expect("lock"), metadata);
}

#[tokio::test]
async fn discover_and_watch_prepend_the_prefix() {
    let backend = Arc::new(RecordingBackend {
        names: Mutex::new(Vec::new()),
        last_metadata: Mutex::new(HashMap::new()),
    });
    let wrapper = scoped(Arc::clone(&backend), "event-broker");
    assert!(
        wrapper
            .discover("delivery", DiscoveryFilter::default())
            .await
            .is_ok()
    );
    assert!(wrapper.watch("delivery").await.is_ok());
    assert_eq!(
        backend.names.lock().expect("lock").as_slice(),
        ["event-broker/delivery", "event-broker/delivery"]
    );
}
