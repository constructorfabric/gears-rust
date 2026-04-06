//! Directory API - contract for service discovery and instance resolution
//!
//! This module defines the core traits and types for the directory service API.

use anyhow::Result;
use async_trait::async_trait;

/// Represents an endpoint where a service can be reached
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServiceEndpoint {
    pub uri: String,
}

impl ServiceEndpoint {
    pub fn new(uri: impl Into<String>) -> Self {
        Self { uri: uri.into() }
    }

    #[must_use]
    pub fn http(host: &str, port: u16) -> Self {
        Self {
            uri: format!("{}://{}:{}", "http", host, port),
        }
    }

    #[must_use]
    pub fn https(host: &str, port: u16) -> Self {
        Self {
            uri: format!("https://{host}:{port}"),
        }
    }

    pub fn uds(path: impl AsRef<std::path::Path>) -> Self {
        Self {
            uri: format!("unix://{}", path.as_ref().display()),
        }
    }
}

/// Information about a service instance
#[derive(Debug, Clone)]
pub struct ServiceInstanceInfo {
    /// Module name this instance belongs to
    pub module: String,
    /// Unique instance identifier
    pub instance_id: String,
    /// Primary endpoint for the instance
    pub endpoint: ServiceEndpoint,
    /// Optional version string
    pub version: Option<String>,
}

/// Information for registering a new module instance
#[derive(Debug, Clone)]
pub struct RegisterInstanceInfo {
    /// Module name
    pub module: String,
    /// Unique instance identifier
    pub instance_id: String,
    /// Map of gRPC service name to endpoint
    pub grpc_services: Vec<(String, ServiceEndpoint)>,
    /// Optional version string
    pub version: Option<String>,
}

/// Directory API trait for service discovery and instance management
///
/// This trait defines the contract for interacting with the module directory.
/// It can be implemented by:
/// - A local implementation that delegates to `ModuleManager`
/// - A gRPC client for out-of-process modules
#[async_trait]
pub trait DirectoryClient: Send + Sync {
    /// Resolve a gRPC service by its logical name to an endpoint
    async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint>;

    /// List all service instances for a given module
    async fn list_instances(&self, module: &str) -> Result<Vec<ServiceInstanceInfo>>;

    /// Register a new module instance with the directory
    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()>;

    /// Deregister a module instance (for graceful shutdown)
    async fn deregister_instance(&self, module: &str, instance_id: &str) -> Result<()>;

    /// Send a heartbeat for a module instance to indicate it's still alive
    async fn send_heartbeat(&self, module: &str, instance_id: &str) -> Result<()>;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
#[path = "api_tests.rs"]
mod tests;
