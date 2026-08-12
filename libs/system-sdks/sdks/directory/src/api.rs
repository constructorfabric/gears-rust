//! Directory API - contract for service discovery and instance resolution
//!
//! This gear defines the core traits and types for the directory service API.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::BTreeMap;

/// An equality-AND label selector (Kubernetes `matchLabels` style).
///
/// An instance matches iff it carries **every** requested `key=value` pair; an
/// empty selector matches all instances of a name. A struct (not a bare map) so
/// future filters (e.g. `matchExpressions`) stay additive.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LabelSelector {
    match_labels: BTreeMap<String, String>,
}

impl LabelSelector {
    /// An empty selector - matches every instance of a name.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a selector from a `matchLabels` map.
    #[must_use]
    pub fn from_match_labels(match_labels: BTreeMap<String, String>) -> Self {
        Self { match_labels }
    }

    /// Add one `key=value` equality requirement (builder).
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.match_labels.insert(key.into(), value.into());
        self
    }

    /// The `matchLabels` equality requirements.
    #[must_use]
    pub fn match_labels(&self) -> &BTreeMap<String, String> {
        &self.match_labels
    }

    /// Whether this selector has no requirements (matches everything).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.match_labels.is_empty()
    }

    /// AND-match: `labels` satisfies this selector iff it carries **every**
    /// requested `key=value` pair. An empty selector always matches.
    #[must_use]
    pub fn matches(&self, labels: &BTreeMap<String, String>) -> bool {
        self.match_labels
            .iter()
            .all(|(k, v)| labels.get(k).is_some_and(|lv| lv == v))
    }
}

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
    /// Optional `OpenAPI` spec (JSON) this instance published, if any.
    pub openapi_spec: Option<String>,
    /// Stable content token for the published `OpenAPI` spec, if any.
    pub openapi_spec_hash: Option<String>,
    /// Map of gRPC service name to endpoint published by this instance.
    ///
    /// Carried back by `list_instances` so a subsequent `register_instance`
    /// (which replaces the entry wholesale) can augment — rather than clobber —
    /// the previously-registered gRPC services when adding a REST endpoint.
    pub grpc_services: Vec<(String, ServiceEndpoint)>,
    /// Consumer-defined, opaque instance labels for within-contract targeting
    /// (`cpt-cf-adr-instance-addressable-discovery` §2), e.g. `shard`, `pod`. The directory only stores and
    /// filters on them via [`resolve_by_labels`](DirectoryClient::resolve_by_labels).
    pub labels: BTreeMap<String, String>,
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
    /// Consumer-defined, opaque instance labels (`cpt-cf-adr-instance-addressable-discovery` §2). MUST survive the
    /// idempotent re-registration path (see `GearManager::register_instance`).
    pub labels: BTreeMap<String, String>,
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

    /// Resolve the instances of `name` matching `selector` (`cpt-cf-adr-instance-addressable-discovery` §6).
    ///
    /// Returns the **full** matching set **regardless of health** (each entry
    /// carrying its `labels`, `instance_id`, and endpoints); zero matches is
    /// `Ok(vec![])`, not an error. Health filtering and "pick one from the set"
    /// are caller-owned policy, not toolkit load balancing.
    ///
    /// The default is `list_instances(name)` filtered by the selector's
    /// equality-AND semantics. Backends may override to push the filter
    /// server-side but MUST preserve these semantics.
    async fn resolve_by_labels(
        &self,
        name: &str,
        selector: &LabelSelector,
    ) -> Result<Vec<ServiceInstanceInfo>> {
        let instances = self.list_instances(name).await?;
        Ok(instances
            .into_iter()
            .filter(|inst| selector.matches(&inst.labels))
            .collect())
    }

    /// List every service instance across all registered gears.
    ///
    /// Used by the edge gateway to discover which gears (and their REST
    /// endpoints) to reverse-proxy. This is a lightweight discovery snapshot:
    /// the returned instances do **not** carry `openapi_spec` — even when the
    /// backing store holds a stored specification. The edge fetches a gear's
    /// document once, on first discovery, via
    /// [`get_openapi_spec`](Self::get_openapi_spec).
    async fn list_all_instances(&self) -> Result<Vec<ServiceInstanceInfo>>;

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
    fn label_selector_and_semantics() {
        let labels = BTreeMap::from([
            ("shard".to_owned(), "1".to_owned()),
            ("az".to_owned(), "a".to_owned()),
        ]);

        // Empty selector matches everything (`cpt-cf-adr-instance-addressable-discovery` §6).
        assert!(LabelSelector::new().matches(&labels));

        // Single requirement present.
        assert!(LabelSelector::new().with("shard", "1").matches(&labels));

        // AND semantics: every requested pair must be present.
        assert!(
            LabelSelector::new()
                .with("shard", "1")
                .with("az", "a")
                .matches(&labels)
        );

        // A requested pair with the wrong value does not match.
        assert!(!LabelSelector::new().with("shard", "2").matches(&labels));

        // A requested key absent from the instance does not match.
        assert!(!LabelSelector::new().with("region", "eu").matches(&labels));
    }

    #[test]
    fn label_selector_builders_and_accessors() {
        // `new()` / default is empty and matches everything.
        assert!(LabelSelector::new().is_empty());
        assert!(LabelSelector::default().match_labels().is_empty());

        // `from_match_labels` round-trips the map and reports non-empty.
        let map = BTreeMap::from([("shard".to_owned(), "3".to_owned())]);
        let sel = LabelSelector::from_match_labels(map.clone());
        assert!(!sel.is_empty());
        assert_eq!(sel.match_labels(), &map);
    }

    #[test]
    fn grpc_service_info_new() {
        let info =
            GrpcServiceInfo::new("payment.v1.PaymentApi", ServiceEndpoint::http("pay", 50051));
        assert_eq!(info.service_name, "payment.v1.PaymentApi");
        assert_eq!(info.endpoint.uri, concat!("http", "://pay:50051"));
    }

    #[test]
    fn directory_sentinels_construct_and_display() {
        let not_found = DirectoryNotFound::new("gear foo");
        assert_eq!(not_found.resource, "gear foo");
        assert_eq!(not_found.to_string(), "directory: not found: gear foo");

        let invalid = DirectoryInvalidArgument::new("bad uuid");
        assert_eq!(invalid.message, "bad uuid");
        assert_eq!(invalid.to_string(), "directory: invalid argument: bad uuid");
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
            labels: BTreeMap::new(),
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
            labels: BTreeMap::new(),
        };

        assert_eq!(info.gear, "billing");
        assert_eq!(
            info.rest_endpoint.as_ref().unwrap().uri,
            concat!("http", "://billing:8080")
        );
        assert!(info.openapi_spec.is_some());
    }
}
