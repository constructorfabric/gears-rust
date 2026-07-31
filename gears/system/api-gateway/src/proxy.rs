//! Directory-driven reverse-proxy sync (embedded edge).
//!
//! When `gateway_proxy.enabled`, the api-gateway becomes a `DirectoryService`
//! consumer: a background task periodically enumerates every registered gear
//! instance (`list_all_instances`) and keeps a
//! [`ProxyRegistry`](toolkit_gateway::ProxyRegistry) in sync so the
//! [`Forwarder`](toolkit_gateway::Forwarder) can reverse-proxy each gear's
//! public routes to its pod.
//!
//! Discovery is **pull-based**: each poll rebuilds the desired set from the
//! directory and diffs it against the currently-registered gears, registering
//! new/updated gears and deregistering ones that have disappeared. This makes
//! the edge self-correcting — a missed registration or a gear restart is
//! reconciled on the next tick.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cf_system_sdks::directory::{DirectoryClient, ServiceInstanceInfo};
use tokio_util::sync::CancellationToken;
use toolkit_gateway::{
    Endpoint, GatewayProvider, GearName, OpenApiSpec, ProxyRegistry, ToolKitGatewayProvider,
};

/// Lower bound on the directory poll cadence (`tokio::time::interval` panics on
/// a zero period; also avoids a hot loop on a misconfigured interval).
const MIN_SYNC_INTERVAL: Duration = Duration::from_secs(1);

/// Spawn the background directory-sync loop. Returns immediately; the task runs
/// until `cancel` fires.
pub fn spawn_directory_sync(
    registry: Arc<ProxyRegistry>,
    directory: Arc<dyn DirectoryClient>,
    interval: Duration,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(directory_sync_loop(registry, directory, interval, cancel))
}

/// Poll the directory on a fixed cadence, reconciling the proxy route table,
/// until `cancel` fires.
async fn directory_sync_loop(
    registry: Arc<ProxyRegistry>,
    directory: Arc<dyn DirectoryClient>,
    interval: Duration,
    cancel: CancellationToken,
) {
    let provider = ToolKitGatewayProvider::new(registry);
    let mut ticker = tokio::time::interval(interval.max(MIN_SYNC_INTERVAL));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = ticker.tick() => reconcile(&provider, directory.as_ref()).await,
        }
    }
    tracing::info!("gateway directory-sync stopping");
}

/// Run one reconcile pass, logging (but swallowing) a directory poll failure so
/// the loop keeps retrying on its cadence.
async fn reconcile(provider: &ToolKitGatewayProvider, directory: &dyn DirectoryClient) {
    if let Err(err) = sync_once(provider, directory).await {
        tracing::warn!(error = %err, "gateway directory-sync poll failed");
    }
}

/// Reconcile the proxy registry against a single directory snapshot.
///
/// # Errors
/// Returns an error only if the directory poll itself fails; per-gear
/// registration problems are logged and skipped so one bad gear cannot stall
/// discovery of the rest.
async fn sync_once(
    provider: &ToolKitGatewayProvider,
    directory: &dyn DirectoryClient,
) -> anyhow::Result<()> {
    let instances = directory.list_all_instances().await?;
    apply_snapshot(provider, directory, instances).await;
    Ok(())
}

/// Register every proxyable gear in `instances`, then prune gears no longer
/// present in the snapshot.
async fn apply_snapshot(
    provider: &ToolKitGatewayProvider,
    directory: &dyn DirectoryClient,
    instances: Vec<ServiceInstanceInfo>,
) {
    // `desired` tracks every gear present in the directory snapshot, independent
    // of whether registration succeeds. This ensures the prune loop below only
    // deregisters gears that have genuinely disappeared from the directory — a
    // transient `register_gear` failure must not drop a still-present gear's
    // existing routes.
    let mut desired: HashSet<String> = HashSet::new();
    for inst in instances {
        desired.insert(inst.gear.clone());
        register_gear(provider, directory, &inst).await;
    }

    for gear in provider.registry().registered_gears() {
        if !desired.contains(gear.as_str())
            && let Err(err) = provider.deregister_routes(&gear).await
        {
            tracing::warn!(gear = %gear, error = %err, "failed to deregister stale gear");
        }
    }
}

/// Register one instance's public routes. A non-proxyable instance (no REST
/// endpoint, unparseable URI) is skipped, and a registration error is logged and
/// swallowed, so one bad gear cannot stall discovery of the rest. Directory
/// presence is tracked by the caller ([`apply_snapshot`]) independently of the
/// outcome here. Multiple instances of a gear collapse to one registration (last
/// write wins); cross-replica load balancing is out of scope.
///
/// The discovery snapshot ([`DirectoryClient::list_all_instances`]) is
/// deliberately spec-less: the full `OpenAPI` document is fetched **once per
/// newly discovered gear** via [`DirectoryClient::get_openapi_spec`]. A gear
/// already present in the registry is left untouched (no re-fetch); an updated
/// spec is picked up when the gear next churns out of and back into the
/// directory.
async fn register_gear(
    provider: &ToolKitGatewayProvider,
    directory: &dyn DirectoryClient,
    inst: &ServiceInstanceInfo,
) {
    let Some(rest) = inst.rest_endpoint.as_ref() else {
        return;
    };
    let Some(endpoint) = parse_endpoint(&inst.gear, &rest.uri) else {
        return;
    };

    let gear = GearName::from(inst.gear.as_str());

    // Already routing this gear: keep its existing routes and skip the spec
    // fetch. Withdrawal is handled by the prune pass in `apply_snapshot`.
    if provider.registry().contains_gear(&gear) {
        return;
    }

    let Some(spec) = fetch_spec(directory, &inst.gear).await else {
        return;
    };

    let spec = OpenApiSpec::SerializedJson(Bytes::from(spec));
    if let Err(err) = provider.register_routes(&gear, spec, &endpoint).await {
        tracing::warn!(gear = %inst.gear, error = %err, "failed to register gear proxy routes");
    }
}

/// Parse a gear's advertised REST endpoint, logging and returning `None` on a
/// malformed URI so the caller can skip the gear.
fn parse_endpoint(gear: &str, uri: &str) -> Option<Endpoint> {
    match Endpoint::parse(uri) {
        Ok(endpoint) => Some(endpoint),
        Err(err) => {
            tracing::warn!(
                gear = %gear,
                uri = %uri,
                error = %err,
                "skipping gear with unparseable REST endpoint",
            );
            None
        }
    }
}

/// Fetch a gear's `OpenAPI` document from the directory, logging and returning
/// `None` on failure (e.g. the gear published no spec) so the caller can skip it.
async fn fetch_spec(directory: &dyn DirectoryClient, gear: &str) -> Option<String> {
    match directory.get_openapi_spec(gear).await {
        Ok(spec) => Some(spec),
        Err(err) => {
            tracing::warn!(gear = %gear, error = %err, "skipping gear: could not fetch OpenAPI spec");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{GearName, ProxyRegistry, ToolKitGatewayProvider, sync_once};
    use std::sync::Arc;

    use cf_system_sdks::directory::{DirectoryClient, RegisterInstanceInfo, ServiceEndpoint};
    use toolkit::directory::LocalDirectoryClient;
    use toolkit::runtime::GearManager;

    fn public_spec(gear: &str, path: &str) -> String {
        serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": gear, "version": "1.0.0" },
            "paths": { path: { "get": { "x-cf-api-visibility": "public", "responses": {} } } },
        })
        .to_string()
    }

    async fn register(dir: &dyn DirectoryClient, gear: &str, uri: &str, spec: String) -> String {
        let instance_id = uuid::Uuid::new_v4().to_string();
        dir.register_instance(RegisterInstanceInfo {
            gear: gear.to_owned(),
            instance_id: instance_id.clone(),
            grpc_services: vec![],
            version: None,
            rest_endpoint: Some(ServiceEndpoint::new(uri)),
            openapi_spec: Some(spec),
        })
        .await
        .unwrap();
        instance_id
    }

    #[tokio::test]
    async fn sync_registers_public_routes_then_prunes_removed_gears() {
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));

        let calc_instance = register(
            dir.as_ref(),
            "calc",
            "http://calc:8080",
            public_spec("calc", "/calc/v1/add"),
        )
        .await;
        register(
            dir.as_ref(),
            "billing",
            "http://billing:9090",
            public_spec("billing", "/billing/v1/pay"),
        )
        .await;

        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));

        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert!(registry.match_path("/calc/v1/add").is_some());
        assert!(registry.match_path("/billing/v1/pay").is_some());
        assert_eq!(registry.gear_count(), 2);

        // Remove the calc instance; its gear disappears from the directory and
        // the next sync prunes it, leaving billing intact.
        dir.deregister_instance("calc", &calc_instance)
            .await
            .unwrap();
        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert!(registry.match_path("/calc/v1/add").is_none());
        assert!(registry.match_path("/billing/v1/pay").is_some());
        assert!(!registry.contains_gear(&GearName::from("calc")));
    }

    #[tokio::test]
    async fn sync_retains_routes_when_reregistration_fails_but_gear_still_present() {
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));

        let good = register(
            dir.as_ref(),
            "calc",
            "http://calc:8080",
            public_spec("calc", "/calc/v1/add"),
        )
        .await;

        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));
        sync_once(&provider, dir.as_ref()).await.unwrap();
        assert!(registry.match_path("/calc/v1/add").is_some());

        // The gear is still present in the directory but now advertises an
        // unparseable (schemeless) REST endpoint, so registration is skipped.
        // Because the gear remains in the snapshot, its existing routes must be
        // retained rather than pruned.
        dir.deregister_instance("calc", &good).await.unwrap();
        register(
            dir.as_ref(),
            "calc",
            "/schemeless-uri",
            public_spec("calc", "/calc/v1/add"),
        )
        .await;
        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert!(registry.match_path("/calc/v1/add").is_some());
        assert!(registry.contains_gear(&GearName::from("calc")));
    }

    #[tokio::test]
    async fn sync_skips_instances_without_rest_or_spec() {
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));

        // gRPC-only gear: no REST endpoint / spec -> not proxied.
        dir.register_instance(RegisterInstanceInfo {
            gear: "worker".to_owned(),
            instance_id: uuid::Uuid::new_v4().to_string(),
            grpc_services: vec![(
                "worker.Svc".to_owned(),
                ServiceEndpoint::new("http://worker:7000"),
            )],
            version: None,
            rest_endpoint: None,
            openapi_spec: None,
        })
        .await
        .unwrap();

        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));
        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert_eq!(registry.gear_count(), 0);
    }

    #[tokio::test]
    async fn sync_skips_gear_with_rest_but_no_published_spec() {
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));

        // The gear advertises a REST endpoint but never published an OpenAPI
        // document. Since the slim discovery snapshot carries no spec, the edge
        // tries to fetch it via `get_openapi_spec`, which fails -> not proxied.
        dir.register_instance(RegisterInstanceInfo {
            gear: "specless".to_owned(),
            instance_id: uuid::Uuid::new_v4().to_string(),
            grpc_services: vec![],
            version: None,
            rest_endpoint: Some(ServiceEndpoint::new("http://specless:8080")),
            openapi_spec: None,
        })
        .await
        .unwrap();

        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));
        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert_eq!(registry.gear_count(), 0);
    }

    #[tokio::test]
    async fn sync_fetches_spec_lazily_from_slim_snapshot() {
        // The discovery snapshot never inlines the spec, yet the edge still
        // registers public routes by fetching the document via `get_openapi_spec`
        // on first discovery.
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));

        register(
            dir.as_ref(),
            "calc",
            "http://calc:8080",
            public_spec("calc", "/calc/v1/add"),
        )
        .await;

        // Precondition: the snapshot is spec-less.
        let snapshot = dir.list_all_instances().await.unwrap();
        assert!(snapshot.iter().all(|i| i.openapi_spec.is_none()));

        let registry = Arc::new(ProxyRegistry::new());
        let provider = ToolKitGatewayProvider::new(Arc::clone(&registry));
        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert!(registry.match_path("/calc/v1/add").is_some());
    }
}
