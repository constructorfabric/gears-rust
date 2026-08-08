//! Directory API - contract for service discovery and instance resolution
//!
//! This gear defines the core traits and types for the directory service API.

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
    /// Gear name this instance belongs to
    pub gear: String,
    /// Unique instance identifier
    pub instance_id: String,
    /// Primary endpoint for the instance
    pub endpoint: ServiceEndpoint,
    /// Optional version string
    pub version: Option<String>,
    /// Optional REST endpoint (HTTP base URL) for this instance.
    /// Not all gears expose a REST API.
    pub rest_endpoint: Option<ServiceEndpoint>,
    /// Map of gRPC service name to endpoint published by this instance.
    ///
    /// Carried back by `list_instances` so a subsequent `register_instance`
    /// (which replaces the entry wholesale) can augment — rather than clobber —
    /// the previously-registered gRPC services when adding a REST endpoint.
    pub grpc_services: Vec<(String, ServiceEndpoint)>,
}

/// Information for registering a new gear instance
#[derive(Debug, Clone)]
pub struct RegisterInstanceInfo {
    /// Gear name
    pub gear: String,
    /// Unique instance identifier
    pub instance_id: String,
    /// Map of gRPC service name to endpoint
    pub grpc_services: Vec<(String, ServiceEndpoint)>,
    /// Optional version string
    pub version: Option<String>,
    /// Optional REST endpoint (HTTP base URL) exposed by the gear.
    pub rest_endpoint: Option<ServiceEndpoint>,
    /// Optional `OpenAPI` spec (JSON) published by the gear.
    pub openapi_spec: Option<String>,
}

/// A resolved gRPC service and the endpoint it is reachable at.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GrpcServiceInfo {
    /// Fully-qualified gRPC service name (e.g. `payment.v1.PaymentApi`).
    pub service_name: String,
    /// Endpoint the service is reachable at.
    pub endpoint: ServiceEndpoint,
}

impl GrpcServiceInfo {
    pub fn new(service_name: impl Into<String>, endpoint: ServiceEndpoint) -> Self {
        Self {
            service_name: service_name.into(),
            endpoint,
        }
    }
}

/// Sentinel error wrapped via `anyhow::Error` to signal "the requested gear or
/// service is not registered (or has no live instance)" through the
/// [`DirectoryClient`] trait. Consumers downcast to this type to distinguish a
/// not-ready provider (eventual readiness) from a directory-backend failure —
/// see `toolkit::discovery::DirectoryEndpointResolver`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryNotFound {
    /// What was being looked up — e.g. `"gear foo"` or `"service foo.Bar"`.
    pub resource: String,
}

impl DirectoryNotFound {
    pub fn new(resource: impl Into<String>) -> Self {
        Self {
            resource: resource.into(),
        }
    }
}

impl std::fmt::Display for DirectoryNotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "directory: not found: {}", self.resource)
    }
}

impl std::error::Error for DirectoryNotFound {}

/// Sentinel error wrapped via `anyhow::Error` to signal "client-supplied
/// argument is malformed" (e.g. invalid UUID) through the [`DirectoryClient`]
/// trait. Allows the gRPC server boundary to return `Status::invalid_argument`
/// instead of mislabeling a client bug as an internal failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryInvalidArgument {
    /// Human-readable description of what was invalid.
    pub message: String,
}

impl DirectoryInvalidArgument {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DirectoryInvalidArgument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "directory: invalid argument: {}", self.message)
    }
}

impl std::error::Error for DirectoryInvalidArgument {}

/// Directory API trait for service discovery and instance management
///
/// This trait defines the contract for interacting with the gear directory.
/// It can be implemented by:
/// - A local implementation that delegates to `GearManager`
/// - A gRPC client for out-of-process gears
#[async_trait]
pub trait DirectoryClient: Send + Sync {
    /// Resolve a gRPC service by its logical name to an endpoint
    async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint>;

    /// Resolve a REST endpoint (HTTP base URL) for a gear by its name.
    ///
    /// Returns the base URL (e.g. `http://billing:8080`) that callers use to
    /// make REST requests to the resolved gear.
    async fn resolve_rest_service(&self, gear_name: &str) -> Result<ServiceEndpoint>;

    /// Retrieve the `OpenAPI` spec (JSON) published by a gear.
    async fn get_openapi_spec(&self, gear_name: &str) -> Result<String>;

    /// List all service instances for a given gear
    async fn list_instances(&self, gear: &str) -> Result<Vec<ServiceInstanceInfo>>;

    /// Register a new gear instance with the directory
    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()>;

    /// Deregister a gear instance (for graceful shutdown)
    async fn deregister_instance(&self, gear: &str, instance_id: &str) -> Result<()>;

    /// Send a heartbeat for a gear instance to indicate it's still alive
    async fn send_heartbeat(&self, gear: &str, instance_id: &str) -> Result<()>;
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_service_endpoint_creation() {
        let http_ep = ServiceEndpoint::http("localhost", 8080);
        assert_eq!(http_ep.uri, concat!("http", "://localhost:8080"));

        let https_endpoint = ServiceEndpoint::https("localhost", 8443);
        assert_eq!(https_endpoint.uri, "https://localhost:8443");

        let uds_ep = ServiceEndpoint::uds("/tmp/socket.sock");
        assert!(uds_ep.uri.starts_with("unix://"));
        assert!(uds_ep.uri.contains("socket.sock"));

        let custom_ep = ServiceEndpoint::new(concat!("http", "://example.com"));
        assert_eq!(custom_ep.uri, concat!("http", "://example.com"));
    }

    #[test]
    fn test_register_instance_info() {
        let info = RegisterInstanceInfo {
            gear: "test_gear".to_owned(),
            instance_id: "instance1".to_owned(),
            grpc_services: vec![(
                "test.Service".to_owned(),
                ServiceEndpoint::http("127.0.0.1", 8001),
            )],
            version: Some("1.0.0".to_owned()),
            rest_endpoint: None,
            openapi_spec: None,
        };

        assert_eq!(info.gear, "test_gear");
        assert_eq!(info.instance_id, "instance1");
        assert_eq!(info.grpc_services.len(), 1);
        assert!(info.rest_endpoint.is_none());
        assert!(info.openapi_spec.is_none());
    }

    #[test]
    fn test_register_instance_info_with_rest() {
        let info = RegisterInstanceInfo {
            gear: "billing".to_owned(),
            instance_id: "instance1".to_owned(),
            grpc_services: vec![],
            version: Some("2.0.0".to_owned()),
            rest_endpoint: Some(ServiceEndpoint::http("billing", 8080)),
            openapi_spec: Some("{\"openapi\":\"3.1.0\"}".to_owned()),
        };

        assert_eq!(info.gear, "billing");
        assert_eq!(
            info.rest_endpoint.as_ref().unwrap().uri,
            concat!("http", "://billing:8080")
        );
        assert!(info.openapi_spec.is_some());
    }
}
