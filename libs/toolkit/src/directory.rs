//! Directory API - contract for service discovery and instance resolution

use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use toolkit_stable_hash::murmur3_x86_32;
use uuid::Uuid;

use crate::runtime::{Endpoint, GearInstance, GearManager};

/// Version tag mixed into the [`openapi_spec_hash`] framing. Bump only on an
/// intentional, breaking change to the token encoding.
const OPENAPI_SPEC_HASH_VERSION: u8 = 1;

/// Distinct seeds for the two 32-bit halves that compose the 64-bit token.
const OPENAPI_SPEC_HASH_SEED_HI: u32 = 0x5bd1_e995;
const OPENAPI_SPEC_HASH_SEED_LO: u32 = 0x1b87_3593;

/// Compute a content token for an `OpenAPI` document, used to detect changes.
///
/// This is a change-detection token (like a k8s `resourceVersion`), **not** a
/// security digest: consumers compare it against a previously observed token for
/// the same spec to decide whether the document changed. A non-cryptographic
/// hash is therefore sufficient.
///
/// Uses the versioned stable [`murmur3_x86_32`] rather than `std`'s
/// `DefaultHasher` so the token is **identical across directory binaries and
/// Rust versions** (`DefaultHasher` guarantees neither). This matches the wire
/// contract — the token is serialized (`ServiceInstanceInfo::openapi_spec_hash`)
/// and consumed by edge clients — so replicas on different builds agree and a
/// client polling multiple replicas sees no spurious change. Inputs use
/// **explicit fixed framing** (a version tag plus a length-prefixed field), and
/// two differently-seeded 32-bit hashes compose the deterministic 64-bit token.
fn openapi_spec_hash(spec: &str) -> String {
    let mut buf = Vec::with_capacity(1 + 8 + spec.len());
    buf.push(OPENAPI_SPEC_HASH_VERSION);
    buf.extend_from_slice(&(spec.len() as u64).to_le_bytes());
    buf.extend_from_slice(spec.as_bytes());

    let hi = murmur3_x86_32(&buf, OPENAPI_SPEC_HASH_SEED_HI);
    let lo = murmur3_x86_32(&buf, OPENAPI_SPEC_HASH_SEED_LO);
    let token = (u64::from(hi) << 32) | u64::from(lo);
    format!("{token:016x}")
}

// Re-export all types from contracts - this is the single source of truth
pub use cf_system_sdks::directory::{
    DirectoryClient, DirectoryInvalidArgument, DirectoryNotFound, GrpcServiceInfo, LabelSelector,
    RegisterInstanceInfo, ServiceEndpoint, ServiceInstanceInfo,
};

/// Project a live [`GearInstance`] into a [`ServiceInstanceInfo`] resolution
/// result. Shared by `list_instances` (`include_spec = true`) and
/// `list_all_instances` (`include_spec = false`, the bounded cross-gear
/// snapshot). Carries `labels` so `resolve_by_labels` has complete data
/// (`cpt-cf-adr-instance-addressable-discovery` §2).
fn instance_to_info(inst: &GearInstance, include_spec: bool) -> ServiceInstanceInfo {
    // Prefer a gRPC endpoint for the primary `endpoint`; fall back to the REST
    // endpoint (OoP gears often register REST-only).
    let endpoint = inst
        .grpc_services
        .values()
        .next()
        .or(inst.rest_endpoint.as_ref())
        .map_or_else(
            || ServiceEndpoint::new(String::new()),
            |ep| ServiceEndpoint::new(ep.uri.clone()),
        );

    ServiceInstanceInfo {
        gear: inst.gear.clone(),
        instance_id: inst.instance_id.to_string(),
        endpoint,
        version: inst.version.clone(),
        rest_endpoint: inst
            .rest_endpoint
            .as_ref()
            .map(|ep| ServiceEndpoint::new(ep.uri.clone())),
        openapi_spec_hash: inst.openapi_spec.as_deref().map(openapi_spec_hash),
        openapi_spec: if include_spec {
            inst.openapi_spec.clone()
        } else {
            None
        },
        // Carry every published gRPC service back so the directory-register
        // phase can augment (not clobber) this instance when it adds a REST
        // endpoint.
        grpc_services: inst
            .grpc_services
            .iter()
            .map(|(name, e)| (name.clone(), ServiceEndpoint::new(e.uri.clone())))
            .collect(),
        labels: inst.labels.clone(),
    }
}

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
    // Every lookup below is an in-memory `GearManager` map read, so `None` can
    // only ever mean "nothing registered under that name" — this client has no
    // backend and therefore no failure mode. Returning the typed
    // `DirectoryNotFound` sentinel rather than a bare `anyhow` is what lets
    // `DirectoryEndpointResolver` report `Ok(None)` ("provider not up yet")
    // instead of `Err` ("the directory is broken"); the difference decides
    // whether a routine startup race is logged at `debug` or `warn`.
    async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint> {
        if let Some((_gear, _inst, ep)) = self.mgr.pick_service_round_robin(service_name) {
            return Ok(ServiceEndpoint::new(ep.uri));
        }

        Err(DirectoryNotFound::new(format!("service {service_name}")).into())
    }

    async fn resolve_rest_service(&self, gear_name: &str) -> Result<ServiceEndpoint> {
        if let Some(ep) = self.mgr.pick_rest_endpoint_round_robin(gear_name) {
            return Ok(ServiceEndpoint::new(ep.uri));
        }

        Err(DirectoryNotFound::new(format!("gear {gear_name}")).into())
    }

    async fn get_openapi_spec(&self, gear_name: &str) -> Result<String> {
        self.mgr.openapi_spec_of(gear_name).ok_or_else(|| {
            DirectoryNotFound::new(format!("openapi spec for gear {gear_name}")).into()
        })
    }

    async fn list_instances(&self, gear: &str) -> Result<Vec<ServiceInstanceInfo>> {
        // Enumeration MUST include **every** instance regardless of transport;
        // the previous "skip instances with no gRPC service" dropped REST-only
        // roles that `resolve_by_labels` must be able to target
        // (`cpt-cf-adr-instance-addressable-discovery`).
        let result = self
            .mgr
            .instances_of(gear)
            .into_iter()
            .map(|inst| instance_to_info(&inst, /* include_spec */ true))
            .collect();

        Ok(result)
    }

    async fn list_all_instances(&self) -> Result<Vec<ServiceInstanceInfo>> {
        let result = self
            .mgr
            .all_instances()
            .into_iter()
            // Omit the OpenAPI document (`include_spec = false`) so the polled
            // cross-gear snapshot stays bounded; the content hash still lets the
            // edge skip unchanged specs.
            .map(|inst| instance_to_info(&inst, /* include_spec */ false))
            .collect();

        Ok(result)
    }

    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()> {
        // Parse instance_id from string to Uuid
        let instance_id = Uuid::parse_str(&info.instance_id)
            .map_err(|e| anyhow::anyhow!("Invalid instance_id '{}': {}", info.instance_id, e))?;

        // Build a GearInstance from RegisterInstanceInfo
        let mut instance = GearInstance::new(info.gear.clone(), instance_id);

        // Apply version if provided
        if let Some(version) = info.version {
            instance = instance.with_version(version);
        }

        // Add all gRPC services
        for (service_name, endpoint) in info.grpc_services {
            instance = instance.with_grpc_service(service_name, Endpoint::from_uri(endpoint.uri));
        }

        // Apply REST endpoint if provided
        if let Some(rest) = info.rest_endpoint {
            instance = instance.with_rest_endpoint(Endpoint::from_uri(rest.uri));
        }

        // Apply OpenAPI spec if provided
        if let Some(spec) = info.openapi_spec {
            instance = instance.with_openapi_spec(spec);
        }

        // Apply labels (`cpt-cf-adr-instance-addressable-discovery` §2);
        // `with_metadata_of` carries them across re-registration.
        if !info.labels.is_empty() {
            instance = instance.with_labels(info.labels);
        }

        // Register the instance with the manager
        self.mgr.register_instance(Arc::new(instance));

        Ok(())
    }

    async fn deregister_instance(&self, gear: &str, instance_id: &str) -> Result<()> {
        let instance_id = Uuid::parse_str(instance_id)
            .map_err(|e| anyhow::anyhow!("Invalid instance_id '{instance_id}': {e}"))?;
        self.mgr.deregister(gear, instance_id);
        Ok(())
    }

    async fn send_heartbeat(&self, gear: &str, instance_id: &str) -> Result<()> {
        let instance_id = Uuid::parse_str(instance_id)
            .map_err(|e| anyhow::anyhow!("Invalid instance_id '{instance_id}': {e}"))?;
        self.mgr
            .update_heartbeat(gear, instance_id, std::time::Instant::now());
        Ok(())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn openapi_spec_hash_known_answer_vectors() {
        // Pinned tokens over representative specs. These MUST stay stable across
        // directory binaries and Rust versions (the token is serialized and
        // compared by edge clients); a change means a deliberate encoding bump
        // (`OPENAPI_SPEC_HASH_VERSION`).
        let vectors: &[&str] = &[
            "",
            "{\"openapi\":\"3.1.0\"}",
            "{\"openapi\":\"3.1.0\",\"info\":{\"title\":\"calc\"}}",
        ];
        let actual: Vec<String> = vectors.iter().map(|s| openapi_spec_hash(s)).collect();
        let expected: Vec<String> = vec![
            "fa85af87e70aade0".to_owned(),
            "88b7d2b224eb6181".to_owned(),
            "78ac723486d5faee".to_owned(),
        ];
        assert_eq!(actual, expected);

        // Tokens are 16 hex chars (zero-padded 64-bit) and change with content.
        assert!(actual.iter().all(|t| t.len() == 16));
        assert_ne!(openapi_spec_hash("a"), openapi_spec_hash("b"));
    }

    #[tokio::test]
    async fn test_resolve_grpc_service_not_found() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir);

        let err = api
            .resolve_grpc_service("nonexistent.Service")
            .await
            .unwrap_err();
        // Asserting `is_err()` alone would pass for a bare `anyhow` too, which
        // is what this client used to return — and which
        // `DirectoryEndpointResolver` reads as "the directory is broken".
        assert!(
            err.downcast_ref::<DirectoryNotFound>().is_some(),
            "expected the typed not-found sentinel, got: {err:?}"
        );
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
            grpc_services: vec![(
                "test.Service".to_owned(),
                ServiceEndpoint::http("127.0.0.1", 8001),
            )],
            version: Some("1.0.0".to_owned()),
            rest_endpoint: None,
            openapi_spec: None,
            labels: std::collections::BTreeMap::new(),
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
    async fn test_register_and_resolve_rest_and_openapi() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        let register_info = RegisterInstanceInfo {
            gear: "billing".to_owned(),
            instance_id: instance_id.to_string(),
            grpc_services: vec![],
            version: Some("1.0.0".to_owned()),
            rest_endpoint: Some(ServiceEndpoint::http("billing", 8080)),
            openapi_spec: Some("{\"openapi\":\"3.1.0\"}".to_owned()),
            labels: std::collections::BTreeMap::new(),
        };

        api.register_instance(register_info).await.unwrap();

        // REST endpoint resolves to the registered base URL.
        let resolved = api.resolve_rest_service("billing").await.unwrap();
        assert_eq!(resolved.uri, concat!("http", "://billing:8080"));

        // OpenAPI spec can be retrieved.
        let spec = api.get_openapi_spec("billing").await.unwrap();
        assert!(spec.contains("openapi"));
    }

    #[tokio::test]
    async fn test_resolve_rest_and_openapi_not_found() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir);

        // The typed sentinel is what distinguishes "provider not up yet" from
        // a directory failure — see `DirectoryEndpointResolver`.
        let rest_err = api.resolve_rest_service("missing").await.unwrap_err();
        assert!(
            rest_err.downcast_ref::<DirectoryNotFound>().is_some(),
            "expected the typed not-found sentinel, got: {rest_err:?}"
        );

        let spec_err = api.get_openapi_spec("missing").await.unwrap_err();
        assert!(
            spec_err.downcast_ref::<DirectoryNotFound>().is_some(),
            "expected the typed not-found sentinel, got: {spec_err:?}"
        );
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
    async fn resolve_by_labels_targets_a_specific_shard() {
        use std::collections::BTreeMap;

        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        // Three shards of one directory name (the `cpt-cf-adr-instance-addressable-discovery` §1 ingest example).
        for shard in ["0", "1", "2"] {
            api.register_instance(RegisterInstanceInfo {
                gear: "event-broker-ingest".to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                grpc_services: vec![],
                version: Some("1.0.0".to_owned()),
                rest_endpoint: Some(ServiceEndpoint::new(format!("http://ingest-{shard}:8080"))),
                openapi_spec: None,
                labels: BTreeMap::from([("shard".to_owned(), shard.to_owned())]),
            })
            .await
            .unwrap();
        }

        // Targeted resolve pinpoints exactly the requested shard (addressing
        // only — no health annotation; liveness is the caller's concern).
        let selector = LabelSelector::new().with("shard", "1");
        let matched = api
            .resolve_by_labels("event-broker-ingest", &selector)
            .await
            .unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(
            matched[0].labels.get("shard").map(String::as_str),
            Some("1")
        );
        assert_eq!(
            matched[0].rest_endpoint.as_ref().map(|e| e.uri.as_str()),
            Some("http://ingest-1:8080")
        );

        // An empty selector matches all instances of the name.
        assert_eq!(
            api.resolve_by_labels("event-broker-ingest", &LabelSelector::new())
                .await
                .unwrap()
                .len(),
            3
        );

        // A non-matching selector yields `Ok(empty)` (not an error).
        assert!(
            api.resolve_by_labels(
                "event-broker-ingest",
                &LabelSelector::new().with("shard", "9")
            )
            .await
            .unwrap()
            .is_empty()
        );
    }

    #[tokio::test]
    async fn list_instances_includes_rest_only_roles() {
        use std::collections::BTreeMap;

        // `cpt-cf-adr-instance-addressable-discovery` enumeration fix: a REST-only instance (no gRPC service) MUST
        // still be enumerated so `resolve_by_labels` can target it.
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));
        api.register_instance(RegisterInstanceInfo {
            gear: "rest-only".to_owned(),
            instance_id: Uuid::new_v4().to_string(),
            grpc_services: vec![],
            version: None,
            rest_endpoint: Some(ServiceEndpoint::http("rest-only", 8080)),
            openapi_spec: None,
            labels: BTreeMap::from([("pod".to_owned(), "rest-only-0".to_owned())]),
        })
        .await
        .unwrap();

        let listed = api.list_instances("rest-only").await.unwrap();
        assert_eq!(listed.len(), 1, "REST-only role must be enumerated");
        assert_eq!(
            listed[0].labels.get("pod").map(String::as_str),
            Some("rest-only-0")
        );
    }

    #[tokio::test]
    async fn labels_survive_reregistration() {
        use std::collections::BTreeMap;

        // `cpt-cf-adr-instance-addressable-discovery` §2: labels MUST survive the idempotent re-registration path
        // (a self-heal register must not drop them).
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));
        let id = Uuid::new_v4();
        let make = |labels: BTreeMap<String, String>| RegisterInstanceInfo {
            gear: "svc".to_owned(),
            instance_id: id.to_string(),
            grpc_services: vec![],
            version: Some("1.0.0".to_owned()),
            rest_endpoint: Some(ServiceEndpoint::http("svc", 8080)),
            openapi_spec: None,
            labels,
        };

        api.register_instance(make(BTreeMap::from([("shard".to_owned(), "7".to_owned())])))
            .await
            .unwrap();
        // Re-register the same instance carrying the same labels (self-heal).
        api.register_instance(make(BTreeMap::from([("shard".to_owned(), "7".to_owned())])))
            .await
            .unwrap();

        let insts = dir.instances_of("svc");
        assert_eq!(insts.len(), 1);
        assert_eq!(insts[0].labels.get("shard").map(String::as_str), Some("7"));
    }

    #[tokio::test]
    async fn test_list_all_instances_across_gears() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        // Two REST-only OoP gears (no gRPC services) + one gRPC-only gear.
        for (gear, port) in [("billing", 8080u16), ("catalog", 8081u16)] {
            api.register_instance(RegisterInstanceInfo {
                gear: gear.to_owned(),
                instance_id: Uuid::new_v4().to_string(),
                grpc_services: vec![],
                version: Some("1.0.0".to_owned()),
                rest_endpoint: Some(ServiceEndpoint::http(gear, port)),
                openapi_spec: Some(format!("{{\"openapi\":\"3.1.0\",\"x\":\"{gear}\"}}")),
                labels: std::collections::BTreeMap::new(),
            })
            .await
            .unwrap();
        }

        // gRPC-only gear: gRPC service metadata and no REST endpoint / spec. This
        // exercises gRPC endpoint selection in `list_all_instances` (which prefers
        // a gRPC endpoint for the primary `endpoint`).
        api.register_instance(RegisterInstanceInfo {
            gear: "reporting".to_owned(),
            instance_id: Uuid::new_v4().to_string(),
            grpc_services: vec![(
                "reporting.Service".to_owned(),
                ServiceEndpoint::new("http://reporting:7000"),
            )],
            version: Some("1.0.0".to_owned()),
            labels: std::collections::BTreeMap::new(),
            rest_endpoint: None,
            openapi_spec: None,
        })
        .await
        .unwrap();

        let all = api.list_all_instances().await.unwrap();
        assert_eq!(all.len(), 3);

        let billing = all.iter().find(|i| i.gear == "billing").expect("billing");
        assert_eq!(
            billing.rest_endpoint.as_ref().map(|e| e.uri.as_str()),
            Some("http://billing:8080")
        );
        // The cross-gear snapshot never inlines the OpenAPI document; consumers
        // fetch it per gear via `get_openapi_spec`.
        assert!(billing.openapi_spec.is_none());
        assert!(
            api.get_openapi_spec("billing")
                .await
                .expect("billing spec")
                .contains("billing")
        );

        // The gRPC-only gear resolves its primary endpoint from gRPC metadata and
        // carries no REST endpoint or OpenAPI spec.
        let reporting = all
            .iter()
            .find(|i| i.gear == "reporting")
            .expect("reporting");
        assert_eq!(reporting.endpoint.uri.as_str(), "http://reporting:7000");
        assert!(reporting.rest_endpoint.is_none());
        assert!(reporting.openapi_spec.is_none());
    }
}
