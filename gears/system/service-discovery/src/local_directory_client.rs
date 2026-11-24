use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use toolkit::directory::{
    DirectoryClient, DirectoryInvalidArgument, DirectoryNotFound, DirectoryServiceNameConflict,
    LabelSelector, RegisterAck, RegisterInstanceInfo, ReportedState, ServiceEndpoint,
    ServiceInstanceInfo, instance_from_registration, project_instance,
};
use toolkit::runtime::{GearInstance, GearManager, GrpcServiceNameConflict};
use uuid::Uuid;

/// In-process [`DirectoryClient`] backed by the host's [`GearManager`].
///
/// The service-discovery gear uses this when no shared directory backend is
/// configured.
pub struct LocalDirectoryClient {
    manager: Arc<GearManager>,
}

impl LocalDirectoryClient {
    #[must_use]
    pub fn new(manager: Arc<GearManager>) -> Self {
        Self { manager }
    }
}

pub fn conflict_error(conflict: GrpcServiceNameConflict) -> anyhow::Error {
    DirectoryServiceNameConflict::from(conflict).into()
}

pub fn parse_instance_id(instance_id: &str) -> Result<Uuid> {
    Uuid::parse_str(instance_id).map_err(|error| {
        DirectoryInvalidArgument::new(format!("invalid instance_id '{instance_id}': {error}"))
            .into()
    })
}

pub fn not_found(gear: &str, instance_id: &str) -> anyhow::Error {
    DirectoryNotFound::new(format!("instance {gear}/{instance_id}")).into()
}

/// Validate a registration request and build the instance it describes.
pub fn registered_instance(info: RegisterInstanceInfo) -> Result<GearInstance> {
    cf_system_sdks::directory::validate_labels(&info.labels)
        .map_err(|error| anyhow::Error::from(DirectoryInvalidArgument::new(error.to_string())))?;
    let id = parse_instance_id(&info.instance_id)?;
    Ok(instance_from_registration(info, id))
}

#[async_trait]
impl DirectoryClient for LocalDirectoryClient {
    async fn resolve_grpc_service(&self, service_name: &str) -> Result<ServiceEndpoint> {
        self.manager
            .pick_service_round_robin(service_name)
            .map(|(_, _, endpoint)| ServiceEndpoint::new(endpoint.uri))
            .ok_or_else(|| DirectoryNotFound::new(format!("service {service_name}")).into())
    }

    async fn resolve_rest_service(&self, gear_name: &str) -> Result<ServiceEndpoint> {
        self.manager
            .pick_rest_endpoint_round_robin(gear_name)
            .map(|endpoint| ServiceEndpoint::new(endpoint.uri))
            .ok_or_else(|| DirectoryNotFound::new(format!("gear {gear_name}")).into())
    }

    async fn list_instances(&self, gear: &str) -> Result<Vec<ServiceInstanceInfo>> {
        Ok(self
            .manager
            .instances_of(gear)
            .iter()
            .map(|instance| project_instance(instance))
            .collect())
    }

    async fn resolve_by_labels(
        &self,
        gear: &str,
        selector: &LabelSelector,
    ) -> Result<Vec<ServiceInstanceInfo>> {
        Ok(self
            .manager
            .instances_of(gear)
            .iter()
            .map(|instance| project_instance(instance))
            .filter(|instance| selector.matches(&instance.labels))
            .collect())
    }

    async fn list_all_instances(&self) -> Result<Vec<ServiceInstanceInfo>> {
        Ok(self
            .manager
            .all_instances()
            .iter()
            .filter_map(|instance| {
                let info = project_instance(instance);
                if info.endpoint.is_none() {
                    tracing::debug!(
                        gear = %instance.gear,
                        instance_id = %instance.instance_id,
                        "skipping instance with no gRPC or REST endpoint from cross-gear snapshot"
                    );
                    return None;
                }
                Some(info.without_labels())
            })
            .collect())
    }

    async fn register_instance(&self, info: RegisterInstanceInfo) -> Result<()> {
        self.manager
            .register_instance(Arc::new(registered_instance(info)?))
            .map_err(conflict_error)
    }

    async fn register_with_lease(&self, info: RegisterInstanceInfo) -> Result<RegisterAck> {
        self.register_instance(info).await?;
        Ok(RegisterAck {
            lease_ttl: Some(self.manager.lease_ttl()),
        })
    }

    async fn deregister_instance(&self, gear: &str, instance_id: &str) -> Result<()> {
        let id = parse_instance_id(instance_id)?;
        self.manager.deregister(gear, id);
        Ok(())
    }

    async fn send_heartbeat(&self, gear: &str, instance_id: &str) -> Result<()> {
        self.heartbeat(gear, instance_id, None).await
    }

    async fn heartbeat(
        &self,
        gear: &str,
        instance_id: &str,
        reported: Option<ReportedState>,
    ) -> Result<()> {
        let id = parse_instance_id(instance_id)?;
        if self.manager.heartbeat(gear, id, reported.map(Into::into)) {
            Ok(())
        } else {
            Err(not_found(gear, instance_id))
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use toolkit::directory::{InstanceState, openapi_spec_hash};
    use toolkit::discovery::EndpointResolver;

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
        let register_info = RegisterInstanceInfo::new("test_gear", instance_id.to_string())
            .with_grpc_services(vec![(
                "test.Service".to_owned(),
                ServiceEndpoint::http("127.0.0.1", 8001),
            )])
            .with_version("1.0.0");

        api.register_instance(register_info).await.unwrap();

        // Verify the instance was registered
        let instances = dir.instances_of("test_gear");
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].instance_id, instance_id);
        assert_eq!(instances[0].version, Some("1.0.0".to_owned()));
        assert!(instances[0].grpc_services.contains_key("test.Service"));
    }

    /// A worker's presence loop and the hub register the same instance, each
    /// with its own half, in any order and again on every self-heal or
    /// retry: neither erases the other's gRPC services or `OpenAPI` spec.
    #[tokio::test]
    async fn presence_and_hub_registrations_keep_each_others_half() {
        for hub_first in [true, false] {
            // A fresh directory per order, so each starts from nothing.
            let api = LocalDirectoryClient::new(Arc::new(GearManager::new()));
            let id = Uuid::new_v4().to_string();
            // What `OopServer`'s presence loop sends.
            let spec_hash = openapi_spec_hash(br#"{"openapi":"3.1.0"}"#);
            let presence = RegisterInstanceInfo::new("worker", &id)
                .with_rest_endpoint(ServiceEndpoint::http("127.0.0.1", 8080))
                .with_openapi_spec_hash(spec_hash.clone());
            // What grpc-hub sends.
            let hub = RegisterInstanceInfo::new("worker", &id)
                .with_grpc_services(vec![(
                    "worker.Svc".to_owned(),
                    ServiceEndpoint::http("127.0.0.1", 50051),
                )])
                .with_version("1.0.0");
            let order = if hub_first {
                [&hub, &presence, &presence, &hub]
            } else {
                [&presence, &hub, &hub, &presence]
            };
            for info in order {
                api.register_instance(info.clone()).await.unwrap();
            }
            let instances = api.list_instances("worker").await.unwrap();
            assert_eq!(instances.len(), 1);
            assert_eq!(instances[0].grpc_services.len(), 1, "gRPC services kept");
            assert!(instances[0].rest_endpoint.is_some(), "REST endpoint kept");
            assert_eq!(
                instances[0].openapi_spec_hash.as_deref(),
                Some(spec_hash.as_str()),
                "OpenAPI spec hash kept"
            );
        }
    }

    #[tokio::test]
    async fn register_rejects_invalid_labels_at_store_boundary() {
        // Validation is enforced at the store, not only at the gRPC front-end:
        // an invalid label registered in-process must be rejected here (typed
        // as DirectoryInvalidArgument) and never reach the manager.
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        let err = api
            .register_instance(
                RegisterInstanceInfo::new("worker", Uuid::new_v4().to_string())
                    .with_rest_endpoint(ServiceEndpoint::new("http://worker:8080"))
                    .with_labels(labels(&[("bad key", "7")])),
            )
            .await
            .expect_err("an invalid label must be rejected at the store boundary");
        assert!(
            err.downcast_ref::<DirectoryInvalidArgument>().is_some(),
            "store-boundary label rejection must be typed InvalidArgument, got: {err}"
        );
        assert!(
            dir.instances_of("worker").is_empty(),
            "a rejected registration must not be stored"
        );
    }

    #[tokio::test]
    async fn test_register_and_resolve_rest_and_openapi() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        let spec_hash = openapi_spec_hash(b"{\"openapi\":\"3.1.0\"}");
        let register_info = RegisterInstanceInfo::new("billing", instance_id.to_string())
            .with_version("1.0.0")
            .with_rest_endpoint(ServiceEndpoint::http("billing", 8080))
            .with_openapi_spec_hash(spec_hash.clone());

        api.register_instance(register_info).await.unwrap();

        // REST endpoint resolves to the registered base URL.
        let resolved = api.resolve_rest_service("billing").await.unwrap();
        assert_eq!(resolved.uri, concat!("http", "://billing:8080"));

        // The OpenAPI spec hash is carried on the instance projection.
        let listed = api.list_instances("billing").await.unwrap();
        assert_eq!(
            listed[0].openapi_spec_hash.as_deref(),
            Some(spec_hash.as_str())
        );
    }

    #[tokio::test]
    async fn test_resolve_rest_not_found() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir);

        // The typed sentinel is what distinguishes "provider not up yet" from
        // a directory failure — see `DirectoryEndpointResolver`.
        let rest_err = api.resolve_rest_service("missing").await.unwrap_err();
        assert!(
            rest_err.downcast_ref::<DirectoryNotFound>().is_some(),
            "expected the typed not-found sentinel, got: {rest_err:?}"
        );
        let resolver = toolkit::DirectoryEndpointResolver::new(Arc::new(api));
        assert_eq!(resolver.resolve_endpoint("missing").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_deregister_instance_via_api() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        // Register an instance first
        let inst = Arc::new(GearInstance::new("test_gear", instance_id));
        dir.register_instance(inst).unwrap();

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
        use toolkit::runtime::InstanceState;

        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(dir.clone());

        let instance_id = Uuid::new_v4();
        // Register an instance first
        let inst = Arc::new(GearInstance::new("test_gear", instance_id));
        dir.register_instance(inst).unwrap();

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
    async fn test_list_all_instances_across_gears() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        // Two REST-only OoP gears (no gRPC services) + one gRPC-only gear.
        for (gear, port) in [("billing", 8080u16), ("catalog", 8081u16)] {
            let hash =
                openapi_spec_hash(format!("{{\"openapi\":\"3.1.0\",\"x\":\"{gear}\"}}").as_bytes());
            api.register_instance(
                RegisterInstanceInfo::new(gear, Uuid::new_v4().to_string())
                    .with_version("1.0.0")
                    .with_rest_endpoint(ServiceEndpoint::http(gear, port))
                    .with_openapi_spec_hash(hash),
            )
            .await
            .unwrap();
        }

        // gRPC-only gear: gRPC service metadata and no REST endpoint / spec. This
        // exercises gRPC endpoint selection in `list_all_instances` (which prefers
        // a gRPC endpoint for the primary `endpoint`).
        api.register_instance(
            RegisterInstanceInfo::new("reporting", Uuid::new_v4().to_string())
                .with_grpc_services(vec![(
                    "reporting.Service".to_owned(),
                    ServiceEndpoint::new("http://reporting:7000"),
                )])
                .with_version("1.0.0"),
        )
        .await
        .unwrap();

        let all = api.list_all_instances().await.unwrap();
        assert_eq!(all.len(), 3);

        let billing = all.iter().find(|i| i.gear == "billing").expect("billing");
        assert_eq!(
            billing.rest_endpoint.as_ref().map(|e| e.uri.as_str()),
            Some("http://billing:8080")
        );
        // The cross-gear snapshot never inlines the OpenAPI document; it carries
        // only the hash, and consumers fetch the document from the instance's
        // `/.well-known/openapi.json`.
        assert!(billing.openapi_spec_hash.is_some());

        // The gRPC-only gear resolves its primary endpoint from gRPC metadata and
        // carries no REST endpoint or OpenAPI spec.
        let reporting = all
            .iter()
            .find(|i| i.gear == "reporting")
            .expect("reporting");
        assert_eq!(
            reporting.endpoint.as_ref().map(|e| e.uri.as_str()),
            Some("http://reporting:7000")
        );
        assert!(reporting.rest_endpoint.is_none());
        assert!(reporting.openapi_spec_hash.is_none());
    }

    fn labels(pairs: &[(&str, &str)]) -> std::collections::BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[tokio::test]
    async fn register_carries_labels_into_list_instances() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        api.register_instance(
            RegisterInstanceInfo::new("worker", Uuid::new_v4().to_string())
                .with_grpc_services(vec![(
                    "worker.Svc".to_owned(),
                    ServiceEndpoint::new("http://worker:7000"),
                )])
                .with_labels(labels(&[("shard", "7")])),
        )
        .await
        .unwrap();

        let listed = api.list_instances("worker").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].labels.get("shard"), Some(&"7".to_owned()));
    }

    #[tokio::test]
    async fn resolve_by_labels_selects_matching_instances() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        for (id, shard) in [("a", "7"), ("b", "8"), ("c", "7")] {
            api.register_instance(
                RegisterInstanceInfo::new("worker", Uuid::new_v4().to_string())
                    .with_grpc_services(vec![(
                        format!("worker.{id}"),
                        ServiceEndpoint::new(format!("http://worker-{id}:7000")),
                    )])
                    .with_labels(labels(&[("shard", shard)])),
            )
            .await
            .unwrap();
        }

        let matched = api
            .resolve_by_labels("worker", &LabelSelector::new().with("shard", "7"))
            .await
            .unwrap();
        assert_eq!(matched.len(), 2, "two instances carry shard=7");
        assert!(
            matched
                .iter()
                .all(|i| i.labels.get("shard") == Some(&"7".to_owned()))
        );
    }

    #[tokio::test]
    async fn labelless_reregister_preserves_labels_through_api() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        let instance_id = Uuid::new_v4().to_string();
        api.register_instance(
            RegisterInstanceInfo::new("worker", instance_id.clone())
                .with_rest_endpoint(ServiceEndpoint::new("http://worker:8080"))
                .with_labels(labels(&[("shard", "7")])),
        )
        .await
        .unwrap();

        // A label-less re-registration (e.g. self-heal) must not wipe the shard.
        api.register_instance(
            RegisterInstanceInfo::new("worker", instance_id)
                .with_rest_endpoint(ServiceEndpoint::new("http://worker:8080"))
                .with_version("2.0.0"),
        )
        .await
        .unwrap();

        let listed = api.list_instances("worker").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].labels.get("shard"),
            Some(&"7".to_owned()),
            "label-less re-registration must not wipe stored labels"
        );

        // And it is still selectable by the shard label.
        let matched = api
            .resolve_by_labels("worker", &LabelSelector::new().with("shard", "7"))
            .await
            .unwrap();
        assert_eq!(matched.len(), 1);
    }

    #[tokio::test]
    async fn resolve_by_labels_carries_live_serving_state() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        let instance_id = Uuid::new_v4();
        api.register_instance(
            RegisterInstanceInfo::new("worker", instance_id.to_string())
                .with_grpc_services(vec![(
                    "worker.Svc".to_owned(),
                    ServiceEndpoint::new("http://worker:7000"),
                )])
                .with_labels(labels(&[("shard", "7")])),
        )
        .await
        .unwrap();

        // A freshly-registered instance is not yet serving.
        let matched = api
            .resolve_by_labels("worker", &LabelSelector::new().with("shard", "7"))
            .await
            .unwrap();
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].state, InstanceState::Registered);
        assert!(!matched[0].state.is_serving());

        // A heartbeat transitions it to Healthy; the state must be projected
        // onto the resolve result so a caller can filter on it.
        assert!(dir.heartbeat("worker", instance_id, None));
        let matched = api
            .resolve_by_labels("worker", &LabelSelector::new().with("shard", "7"))
            .await
            .unwrap();
        assert_eq!(matched[0].state, InstanceState::Healthy);
        assert!(matched[0].state.is_serving());
    }

    #[tokio::test]
    async fn resolve_by_labels_omits_openapi_spec() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        api.register_instance(
            RegisterInstanceInfo::new("worker", Uuid::new_v4().to_string())
                .with_rest_endpoint(ServiceEndpoint::new("http://worker:8080"))
                .with_openapi_spec_hash(openapi_spec_hash(b"{\"openapi\":\"3.1.0\"}"))
                .with_labels(labels(&[("shard", "7")])),
        )
        .await
        .unwrap();

        // The in-process label path is spec-free, matching the gRPC client.
        let matched = api
            .resolve_by_labels("worker", &LabelSelector::new().with("shard", "7"))
            .await
            .unwrap();
        assert_eq!(matched.len(), 1);
        assert!(
            matched[0].openapi_spec_hash.is_some(),
            "in-process resolve_by_labels carries only the spec hash, never the document"
        );

        // The plain enumeration path is spec-free too: only the hash rides
        // along, the document is fetched from the instance.
        let listed = api.list_instances("worker").await.unwrap();
        assert!(
            listed[0].openapi_spec_hash.is_some(),
            "list_instances must carry the spec hash, never the document"
        );
    }

    #[tokio::test]
    async fn register_stores_only_the_openapi_spec_hash() {
        // The worker advertises only the hash and serves the document itself;
        // the directory stores the hash verbatim and keeps no document.
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        let hash = openapi_spec_hash(b"{\"openapi\":\"3.1.0\"}");
        api.register_instance(
            RegisterInstanceInfo::new("billing", Uuid::new_v4().to_string())
                .with_rest_endpoint(ServiceEndpoint::new("http://billing:8080"))
                .with_openapi_spec_hash(hash.clone()),
        )
        .await
        .unwrap();

        let listed = api.list_instances("billing").await.unwrap();
        assert_eq!(listed[0].openapi_spec_hash.as_deref(), Some(hash.as_str()));
    }

    #[tokio::test]
    async fn list_all_instances_skips_endpoint_less() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        // One instance advertises a REST endpoint; the other advertises neither
        // gRPC nor REST and must be skipped (no empty-URI sentinel).
        api.register_instance(
            RegisterInstanceInfo::new("worker", Uuid::new_v4().to_string())
                .with_rest_endpoint(ServiceEndpoint::new("http://worker:8080")),
        )
        .await
        .unwrap();
        api.register_instance(RegisterInstanceInfo::new(
            "placeholder",
            Uuid::new_v4().to_string(),
        ))
        .await
        .unwrap();

        let all = api.list_all_instances().await.unwrap();
        assert_eq!(all.len(), 1, "the endpoint-less instance is skipped");
        assert_eq!(all[0].gear, "worker");
        assert!(
            all.iter()
                .all(|i| i.endpoint.as_ref().is_some_and(|e| !e.uri.is_empty())),
            "no instance may carry an absent or empty-URI endpoint"
        );
    }

    #[tokio::test]
    async fn label_path_returns_endpoint_less_match() {
        let dir = Arc::new(GearManager::new());
        let api = LocalDirectoryClient::new(Arc::clone(&dir));

        // A matched instance that has not advertised any endpoint yet: treated
        // as no-endpoint (not an error), so the label-targeting paths must still
        // return the match (endpoint absent) rather than dropping it — the
        // caller owns the fall-back decision.
        api.register_instance(
            RegisterInstanceInfo::new("worker", Uuid::new_v4().to_string())
                .with_labels(labels(&[("shard", "7")])),
        )
        .await
        .unwrap();

        let matched = api
            .resolve_by_labels("worker", &LabelSelector::new().with("shard", "7"))
            .await
            .unwrap();
        assert_eq!(
            matched.len(),
            1,
            "resolve_by_labels must return a matched instance even with no endpoint"
        );
        assert!(
            matched[0].endpoint.is_none(),
            "a no-endpoint match carries endpoint = None, not an empty-URI sentinel"
        );
        assert!(matched[0].rest_endpoint.is_none());
        assert_eq!(matched[0].labels.get("shard"), Some(&"7".to_owned()));

        // The base per-gear enumeration upholds the same contract.
        let listed = api.list_instances("worker").await.unwrap();
        assert_eq!(
            listed.len(),
            1,
            "list_instances must not drop endpoint-less instances"
        );
        assert!(listed[0].endpoint.is_none());
    }
}
