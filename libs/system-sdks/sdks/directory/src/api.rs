//! Directory API - contract for service discovery and instance resolution
//!
//! This gear defines the core traits and types for the directory service API.

use anyhow::Result;
use async_trait::async_trait;

/// Sentinel error wrapped via `anyhow::Error` to signal "no such service /
/// instance" through the [`DirectoryClient`] trait. Allows the gRPC server
/// boundary to return `Status::not_found` for genuine misses while leaving
/// internal failures (DB errors, network glitches in the underlying impl)
/// surfacing as `Status::unavailable` / `Status::internal`.
///
/// Returned by [`anyhow::Error::downcast_ref`] at the boundary:
///
/// ```ignore
/// match err.downcast_ref::<DirectoryNotFound>() {
///     Some(_) => Status::not_found(...),
///     None    => Status::internal(...),
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryNotFound {
    /// What was being looked up — `"gear foo"` or `"service foo.Bar"`.
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GrpcServiceInfo {
    pub service_name: String,
    pub endpoint: ServiceEndpoint,
}

/// Information about a service instance
#[derive(Debug, Clone)]
pub struct ServiceInstanceInfo {
    /// Gear name this instance belongs to
    pub gear: String,
    /// Unique instance identifier
    pub instance_id: String,
    /// Published gRPC services for the instance
    pub grpc_services: Vec<GrpcServiceInfo>,
    /// Optional version string
    pub version: Option<String>,
    /// Optional REST endpoint for this instance (not all modules expose REST)
    pub rest_endpoint: Option<ServiceEndpoint>,
}

/// Information for registering a new gear instance
#[derive(Debug, Clone)]
pub struct RegisterInstanceInfo {
    /// Gear name
    pub gear: String,
    /// Unique instance identifier
    pub instance_id: String,
    /// Published gRPC services for the instance
    pub grpc_services: Vec<GrpcServiceInfo>,
    /// Optional version string
    pub version: Option<String>,
    /// Optional REST endpoint for this instance (not all modules expose REST)
    pub rest_endpoint: Option<ServiceEndpoint>,
}

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

    /// Resolve a REST endpoint for a gear by its name.
    /// Returns the base URL (e.g., `http://billing-service:8080`) for making REST calls.
    async fn resolve_rest_service(&self, gear_name: &str) -> Result<ServiceEndpoint>;

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
            grpc_services: vec![GrpcServiceInfo {
                service_name: "test.Service".to_owned(),
                endpoint: ServiceEndpoint::http("127.0.0.1", 8001),
            }],
            version: Some("1.0.0".to_owned()),
            rest_endpoint: None,
        };

        assert_eq!(info.gear, "test_gear");
        assert_eq!(info.instance_id, "instance1");
        assert_eq!(info.grpc_services.len(), 1);
        assert_eq!(info.grpc_services[0].service_name, "test.Service");
        assert!(info.rest_endpoint.is_none());
    }

    #[test]
    fn test_register_instance_info_with_rest_endpoint() {
        let info = RegisterInstanceInfo {
            gear: "billing".to_owned(),
            instance_id: "instance1".to_owned(),
            grpc_services: vec![],
            version: Some("2.0.0".to_owned()),
            rest_endpoint: Some(ServiceEndpoint::http("billing-service", 8080)),
        };

        assert_eq!(info.gear, "billing");
        let rest_ep = info.rest_endpoint.as_ref().unwrap();
        assert_eq!(rest_ep.uri, concat!("http", "://billing-service:8080"));
    }
}
