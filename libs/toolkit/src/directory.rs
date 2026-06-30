//! Directory API - contract for service discovery and instance resolution

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;

use crate::runtime::{Endpoint, GearInstance, GearManager};

// Re-export all types from contracts - this is the single source of truth
pub use cf_system_sdks::directory::{
    DirectoryClient, DirectoryInvalidArgument, DirectoryNotFound, GrpcServiceInfo,
    RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo,
};

/// Local implementation of `DirectoryClient` that delegates to `GearManager`
///
/// This is the in-process implementation used by gears running in the same
/// process as the gear orchestrator.
pub struct LocalDirectoryClient {
    mgr: Arc<GearManager>,
}

impl LocalDirectoryClient {
    #[must_use]
    pub fn new(mgr: Arc<GearManager>) -> Self {
        Self { mgr }
    }
}

#[async_trait]
impl DirectoryClient for LocalDirectoryClient {
    async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint> {
        if let Some((_gear, _inst, ep)) = self.mgr.pick_service_round_robin(service_name) {
            return Ok(ServiceEndpoint::new(ep.uri));
        }
        // Return the typed `DirectoryNotFound` sentinel via anyhow::Error so
        // the gRPC server boundary can distinguish a real miss from an
        // internal failure via `err.downcast_ref::<DirectoryNotFound>()`.
        Err(anyhow::Error::new(DirectoryNotFound::new(format!(
            "service {service_name}"
        ))))
    }

    async fn resolve_rest_service(&self, gear_name: &str) -> Result<ServiceEndpoint> {
        if let Some((_inst, ep)) = self.mgr.pick_rest_gear(gear_name) {
            return Ok(ServiceEndpoint::new(ep.uri));
        }
        Err(anyhow::Error::new(DirectoryNotFound::new(format!(
            "gear {gear_name} (REST endpoint)"
        ))))
    }

    async fn list_instances(&self, gear: &str) -> Result<Vec<ServiceInstanceInfo>> {
        let mut result = Vec::new();

        for inst in self.mgr.instances_of(gear) {
            if inst.grpc_services.is_empty() && inst.rest_endpoint.is_none() {
                continue;
            }

            result.push(ServiceInstanceInfo {
                gear: gear.to_owned(),
                instance_id: inst.instance_id.to_string(),
                grpc_services: inst
                    .grpc_services
                    .iter()
                    .map(|(service_name, endpoint)| GrpcServiceInfo {
                        service_name: service_name.clone(),
                        endpoint: ServiceEndpoint::new(endpoint.uri.clone()),
                    })
                    .collect(),
                version: inst.version.clone(),
                rest_endpoint: inst
                    .rest_endpoint
                    .as_ref()
                    .map(|re| ServiceEndpoint::new(re.uri.clone())),
            });
        }

        Ok(result)
    }

    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()> {
        // Parse instance_id from string to Uuid
        let instance_id = Uuid::parse_str(&info.instance_id).map_err(|e| {
            anyhow::Error::new(DirectoryInvalidArgument::new(format!(
                "Invalid instance_id '{}': {}",
                info.instance_id, e
            )))
        })?;

        // Build a GearInstance from RegisterInstanceInfo
        let mut instance = GearInstance::new(info.gear.clone(), instance_id);

        // Apply version if provided
        if let Some(version) = info.version {
            instance = instance.with_version(version);
        }

        // Add all gRPC services
        for service in info.grpc_services {
            let service_name = service.service_name;
            let endpoint = service.endpoint;
            instance = instance.with_grpc_service(service_name, Endpoint::from_uri(endpoint.uri));
        }

        // Apply REST endpoint if provided
        if let Some(rest_ep) = info.rest_endpoint {
            instance = instance.with_rest_endpoint(Endpoint::from_uri(rest_ep.uri));
        }

        // Register the instance with the manager
        self.mgr.register_instance(Arc::new(instance));

        Ok(())
    }

    async fn deregister_instance(&self, gear: &str, instance_id: &str) -> Result<()> {
        let instance_id = Uuid::parse_str(instance_id).map_err(|e| {
            anyhow::Error::new(DirectoryInvalidArgument::new(format!(
                "Invalid instance_id '{instance_id}': {e}"
            )))
        })?;
        self.mgr.deregister(gear, instance_id);
        Ok(())
    }

    async fn send_heartbeat(&self, gear: &str, instance_id: &str) -> Result<()> {
        let instance_id = Uuid::parse_str(instance_id).map_err(|e| {
            anyhow::Error::new(DirectoryInvalidArgument::new(format!(
                "Invalid instance_id '{instance_id}': {e}"
            )))
        })?;
        self.mgr
            .update_heartbeat(gear, instance_id, std::time::Instant::now());
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_resolve_grpc_service_not_found() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir);

        let result = api.resolve_grpc_service("nonexistent.Service").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_register_instance_via_api() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        // Register an instance through the API
        let register_info = RegisterInstanceInfo {
            gear: "test_gear".to_owned(),
            instance_id: instance_id.to_string(),
            grpc_services: vec![GrpcServiceInfo {
                service_name: "test.Service".to_owned(),
                endpoint: ServiceEndpoint::http("127.0.0.1", 8001),
            }],
            version: Some("1.0.0".to_owned()),
            rest_endpoint: None,
        };

        api.register_instance(register_info).await.unwrap();

        // Verify the instance was registered
        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].instance_id, instance_id);
        assert_eq!(instances[0].version, Some("1.0.0".to_owned()));
        assert!(instances[0].grpc_services.contains_key("test.Service"));
    }

    #[tokio::test]
    async fn test_deregister_instance_via_api() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        // Register an instance first
        let inst = Arc::new(GearInstance::new("test_gear", instance_id));
        dir.register_instance(inst);

        // Verify it exists
        assert_eq!(dir.instances_of("test_gear").len(), 1);

        // Deregister via API
        api.deregister_instance("test_gear", &instance_id.to_string())
            .await
            .unwrap();

        // Verify it's gone
        assert_eq!(dir.instances_of("test_gear").len(), 0);
    }

    #[tokio::test]
    async fn test_send_heartbeat_via_api() {
        use crate::runtime::InstanceState;

        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        // Register an instance first
        let inst = Arc::new(GearInstance::new("test_gear", instance_id));
        dir.register_instance(inst);

        // Verify initial state is Registered
        let instances = dir.instances_of("test_gear");
        assert_eq!(instances[0].state(), InstanceState::Registered);

        // Send heartbeat via API
        api.send_heartbeat("test_gear", &instance_id.to_string())
            .await
            .unwrap();

        // Verify state transitioned to Healthy
        let instances = dir.instances_of("test_gear");
        assert_eq!(instances[0].state(), InstanceState::Healthy);
    }

    #[tokio::test]
    async fn test_resolve_rest_service_not_found() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir);

        let result = api.resolve_rest_service("nonexistent_module").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_resolve_rest_service_found() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        let inst = Arc::new(
            GearInstance::new("billing", instance_id)
                .with_rest_endpoint(Endpoint::http("billing-service", 8080)),
        );
        dir.register_instance(inst);

        // Mark as healthy so round-robin prefers it
        dir.update_heartbeat("billing", instance_id, std::time::Instant::now());

        let result = api.resolve_rest_service("billing").await;
        assert!(result.is_ok());
        let ep = result.unwrap();
        assert_eq!(ep.uri, "http://billing-service:8080");
    }

    #[tokio::test]
    async fn test_register_instance_with_rest_endpoint() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        let register_info = RegisterInstanceInfo {
            gear: "billing".to_owned(),
            instance_id: instance_id.to_string(),
            grpc_services: vec![GrpcServiceInfo {
                service_name: "billing.BillingService".to_owned(),
                endpoint: ServiceEndpoint::http("127.0.0.1", 9001),
            }],
            version: Some("1.0.0".to_owned()),
            rest_endpoint: Some(ServiceEndpoint::http("127.0.0.1", 8080)),
        };

        api.register_instance(register_info).await.unwrap();

        // Verify the instance was registered with REST endpoint
        let instances = dir.instances_of("billing");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].instance_id, instance_id);
        assert!(instances[0].rest_endpoint.is_some());
        assert_eq!(
            instances[0].rest_endpoint.as_ref().unwrap().uri,
            "http://127.0.0.1:8080"
        );
    }

    #[tokio::test]
    async fn test_list_instances_includes_rest_endpoint() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        let inst = Arc::new(
            GearInstance::new("billing", instance_id)
                .with_grpc_service("billing.Service", Endpoint::http("127.0.0.1", 9001))
                .with_rest_endpoint(Endpoint::http("127.0.0.1", 8080)),
        );
        dir.register_instance(inst);

        let instances = api.list_instances("billing").await.unwrap();
        assert_eq!(instances.len(), 1);
        assert!(instances[0].rest_endpoint.is_some());
        assert_eq!(instances[0].grpc_services.len(), 1);
        assert_eq!(
            instances[0].grpc_services[0].service_name,
            "billing.Service"
        );
        assert_eq!(
            instances[0].rest_endpoint.as_ref().unwrap().uri,
            "http://127.0.0.1:8080"
        );
    }

    #[tokio::test]
    async fn test_list_instances_includes_rest_only_instance() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        let inst = Arc::new(
            GearInstance::new("rest-only", instance_id)
                .with_rest_endpoint(Endpoint::http("127.0.0.1", 8088)),
        );
        dir.register_instance(inst);

        let instances = api.list_instances("rest-only").await.unwrap();
        assert_eq!(instances.len(), 1);
        assert!(instances[0].grpc_services.is_empty());
        assert_eq!(
            instances[0].rest_endpoint.as_ref().unwrap().uri,
            "http://127.0.0.1:8088"
        );
    }
}
