//! Directory API - contract for service discovery and instance resolution

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;

use crate::runtime::{Endpoint, ModuleInstance, ModuleManager};

// Re-export all types from contracts - this is the single source of truth
pub use cf_system_sdks::directory::{
    DirectoryClient, RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo,
};

/// Local implementation of `DirectoryClient` that delegates to `ModuleManager`
///
/// This is the in-process implementation used by modules running in the same
/// process as the module orchestrator.
pub struct LocalDirectoryClient {
    mgr: Arc<ModuleManager>,
}

impl LocalDirectoryClient {
    #[must_use]
    pub fn new(mgr: Arc<ModuleManager>) -> Self {
        Self { mgr }
    }
}

#[async_trait]
impl DirectoryClient for LocalDirectoryClient {
    async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint> {
        if let Some((_module, _inst, ep)) = self.mgr.pick_service_round_robin(service_name) {
            return Ok(ServiceEndpoint::new(ep.uri));
        }

        anyhow::bail!("Service not found or no healthy instances: {service_name}")
    }

    async fn list_instances(&self, module: &str) -> Result<Vec<ServiceInstanceInfo>> {
        let mut result = Vec::new();

        for inst in self.mgr.instances_of(module) {
            if let Some((_, ep)) = inst.grpc_services.iter().next() {
                result.push(ServiceInstanceInfo {
                    module: module.to_owned(),
                    instance_id: inst.instance_id.to_string(),
                    endpoint: ServiceEndpoint::new(ep.uri.clone()),
                    version: inst.version.clone(),
                });
            }
        }

        Ok(result)
    }

    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()> {
        // Parse instance_id from string to Uuid
        let instance_id = Uuid::parse_str(&info.instance_id)
            .map_err(|e| anyhow::anyhow!("Invalid instance_id '{}': {}", info.instance_id, e))?;

        // Build a ModuleInstance from RegisterInstanceInfo
        let mut instance = ModuleInstance::new(info.module.clone(), instance_id);

        // Apply version if provided
        if let Some(version) = info.version {
            instance = instance.with_version(version);
        }

        // Add all gRPC services
        for (service_name, endpoint) in info.grpc_services {
            instance = instance.with_grpc_service(service_name, Endpoint::from_uri(endpoint.uri));
        }

        // Register the instance with the manager
        self.mgr.register_instance(Arc::new(instance));

        Ok(())
    }

    async fn deregister_instance(&self, module: &str, instance_id: &str) -> Result<()> {
        let instance_id = Uuid::parse_str(instance_id)
            .map_err(|e| anyhow::anyhow!("Invalid instance_id '{instance_id}': {e}"))?;
        self.mgr.deregister(module, instance_id);
        Ok(())
    }

    async fn send_heartbeat(&self, module: &str, instance_id: &str) -> Result<()> {
        let instance_id = Uuid::parse_str(instance_id)
            .map_err(|e| anyhow::anyhow!("Invalid instance_id '{instance_id}': {e}"))?;
        self.mgr
            .update_heartbeat(module, instance_id, std::time::Instant::now());
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "directory_tests.rs"]
mod tests;
