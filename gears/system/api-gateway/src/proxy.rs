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
    apply_snapshot(provider, instances).await;
    Ok(())
}

/// Register every proxyable gear in `instances`, then prune gears no longer
/// present in the snapshot.
async fn apply_snapshot(provider: &ToolKitGatewayProvider, instances: Vec<ServiceInstanceInfo>) {
    let mut desired: HashSet<String> = HashSet::new();
    for inst in instances {
        if let Some(gear) = register_gear(provider, &inst).await {
            desired.insert(gear);
        }
    }

    for gear in provider.registry().registered_gears() {
        if !desired.contains(gear.as_str())
            && let Err(err) = provider.deregister_routes(&gear).await
        {
            tracing::warn!(gear = %gear, error = %err, "failed to deregister stale gear");
        }
    }
}

/// Register one instance's public routes. Returns the gear name on success, or
/// `None` if the instance is not proxyable (missing REST endpoint / spec, bad
/// URI) or registration failed. Multiple instances of a gear collapse to one
/// registration (last write wins); cross-replica load balancing is out of scope.
async fn register_gear(
    provider: &ToolKitGatewayProvider,
    inst: &ServiceInstanceInfo,
) -> Option<String> {
    let rest = inst.rest_endpoint.as_ref()?;
    let spec = inst.openapi_spec.as_ref()?;

    let endpoint = match Endpoint::parse(&rest.uri) {
        Ok(endpoint) => endpoint,
        Err(err) => {
            tracing::warn!(
                gear = %inst.gear,
                uri = %rest.uri,
                error = %err,
                "skipping gear with unparseable REST endpoint",
            );
            return None;
        }
    };

    let gear = GearName::from(inst.gear.as_str());
    let spec = OpenApiSpec::SerializedJson(Bytes::from(spec.clone()));
    match provider.register_routes(&gear, spec, &endpoint).await {
        Ok(()) => Some(inst.gear.clone()),
        Err(err) => {
            tracing::warn!(gear = %inst.gear, error = %err, "failed to register gear proxy routes");
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
}
