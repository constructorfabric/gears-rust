//! Directory-driven reverse-proxy sync (embedded edge).
//!
//! When `gateway_proxy.enabled`, the api-gateway becomes a `DirectoryService`
//! consumer: a background task periodically enumerates every registered gear
//! instance (`list_all_instances`) and keeps a
//! [`ProxyRegistry`](toolkit_gateway::ProxyRegistry) in sync so the
//! [`Forwarder`](toolkit_gateway::Forwarder) can reverse-proxy each gear's
//! public routes to its pod.
//!
//! Discovery is **pull-based**: each poll rebuilds the desired set of gear
//! *instances* from the directory and reconciles it against the currently
//! registered instances in a single batched apply (one router rebuild per
//! poll) — registering/refreshing present instances and deregistering ones that
//! have disappeared. This makes the edge self-correcting: a missed registration
//! or a gear restart is reconciled on the next tick.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cf_system_sdks::directory::{DirectoryClient, ServiceInstanceInfo};
use tokio_util::sync::CancellationToken;
use toolkit_gateway::{
    Endpoint, GearInstance, GearName, InstanceSpec, OpenApiSpec, ProxyRegistry,
    ToolKitGatewayProvider,
};

/// Lower bound on the directory poll cadence (`tokio::time::interval` panics on
/// a zero period; also avoids a hot loop on a misconfigured interval).
const MIN_SYNC_INTERVAL: Duration = Duration::from_secs(1);

/// Upper bound on a single reconcile pass. A hung `list_all_instances` or
/// `get_openapi_spec` call is abandoned after this so it can neither block
/// subsequent ticks nor stall shutdown.
const SYNC_TIMEOUT: Duration = Duration::from_secs(30);

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
            _ = ticker.tick() => {
                if reconcile_pass(&provider, directory.as_ref(), &cancel).await.is_break() {
                    break;
                }
            }
        }
    }
    tracing::info!("gateway directory-sync stopping");
}

/// Run a single reconcile pass, bounded by `SYNC_TIMEOUT` and cancel-aware.
///
/// A hung `list_all_instances`/`get_openapi_spec` call is abandoned after the
/// timeout so it can neither block the next tick nor stall shutdown; a fired
/// `cancel` short-circuits the pass immediately. Returns
/// [`ControlFlow::Break`] when the loop should stop.
async fn reconcile_pass(
    provider: &ToolKitGatewayProvider,
    directory: &dyn DirectoryClient,
    cancel: &CancellationToken,
) -> std::ops::ControlFlow<()> {
    tokio::select! {
        () = cancel.cancelled() => std::ops::ControlFlow::Break(()),
        result = tokio::time::timeout(SYNC_TIMEOUT, reconcile(provider, directory)) => {
            if result.is_err() {
                tracing::warn!(
                    timeout_secs = SYNC_TIMEOUT.as_secs(),
                    "gateway directory-sync reconcile timed out; abandoning pass",
                );
            }
            std::ops::ControlFlow::Continue(())
        }
    }
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

/// Reconcile the whole directory snapshot into the proxy route table in a
/// single pass: collect every proxyable instance's routes, compute which
/// registered instances have disappeared, and apply both in one batched
/// registry update (one router rebuild per poll rather than one per instance).
async fn apply_snapshot(
    provider: &ToolKitGatewayProvider,
    directory: &dyn DirectoryClient,
    instances: Vec<ServiceInstanceInfo>,
) {
    // `desired` tracks every (gear, instance_id) present in the directory
    // snapshot, independent of whether route extraction succeeds. This ensures
    // the prune step below only removes instances that have genuinely
    // disappeared from the directory — a transient failure to resolve a
    // still-present instance must not drop its existing routes.
    let mut desired: HashSet<GearInstance> = HashSet::new();
    // Cache OpenAPI specs per gear so multiple instances of the same gear do
    // not trigger redundant directory fetches during one sync pass. `Bytes` is
    // cheap to clone (ref-counted), so co-located instances share one copy.
    let mut specs: HashMap<GearName, Bytes> = HashMap::new();
    let mut to_register: Vec<InstanceSpec<'_>> = Vec::with_capacity(instances.len());

    for inst in instances {
        let gear = GearName::from(inst.gear.as_str());
        desired.insert(GearInstance::new(gear.clone(), &inst.instance_id));

        let Some(spec) = resolve_instance_spec(directory, &inst, &gear, &mut specs).await else {
            continue;
        };
        to_register.push(spec);
    }

    let registered = provider.registry().registered_instances();
    // Safety valve: a successful-but-empty snapshot while instances are still
    // registered would otherwise prune *every* route at once. That pattern is
    // far more likely a transient directory hiccup (restart/partial outage)
    // than every gear genuinely vanishing in one poll, so skip deregistration
    // this pass and let the next poll reconcile. Registrations still apply.
    let removals: Vec<GearInstance> = if desired.is_empty() && !registered.is_empty() {
        tracing::warn!(
            registered = registered.len(),
            "directory snapshot is empty while instances are registered; \
             skipping deregistration this poll to avoid mass route withdrawal",
        );
        Vec::new()
    } else {
        registered
            .into_iter()
            .filter(|instance| !desired.contains(instance))
            .collect()
    };

    provider.apply_snapshot(to_register, &removals);
}

/// Resolve one instance into an [`InstanceSpec`] ready for registration, or
/// `None` if it is not proxyable (no REST endpoint, unparseable URI, or no
/// published `OpenAPI` document). Skips are logged by the helpers so one bad
/// gear cannot stall discovery of the rest.
///
/// Multiple instances of the same gear can be registered side-by-side; the
/// instance-keyed [`ProxyRegistry`] retains every instance. Profile 3 selects a
/// single, stable endpoint per gear and delegates cross-replica load balancing
/// to the k8s Service DNS; retaining all instances is the seam for Profile 2
/// multi-worker selection.
///
/// The discovery snapshot ([`DirectoryClient::list_all_instances`]) is
/// deliberately spec-less: the full `OpenAPI` document is fetched **once per gear**
/// within a single sync pass and reused for all instances of that gear.
async fn resolve_instance_spec(
    directory: &dyn DirectoryClient,
    inst: &ServiceInstanceInfo,
    gear: &GearName,
    specs: &mut HashMap<GearName, Bytes>,
) -> Option<InstanceSpec<'static>> {
    let rest = inst.rest_endpoint.as_ref()?;
    let endpoint = parse_endpoint(gear.as_str(), &rest.uri)?;
    let spec = fetch_or_get_spec(directory, gear, specs).await?;

    Some(InstanceSpec {
        gear: gear.clone(),
        instance_id: inst.instance_id.clone(),
        spec: OpenApiSpec::SerializedJson(spec),
        endpoint,
    })
}

/// Fetch the `OpenAPI` spec for `gear` once per sync pass, returning a cheap
/// (ref-counted) clone of the cached [`Bytes`] if already fetched this pass.
async fn fetch_or_get_spec(
    directory: &dyn DirectoryClient,
    gear: &GearName,
    specs: &mut HashMap<GearName, Bytes>,
) -> Option<Bytes> {
    if let Some(spec) = specs.get(gear) {
        return Some(spec.clone());
    }
    let spec = Bytes::from(fetch_spec(directory, gear.as_str()).await?);
    specs.insert(gear.clone(), spec.clone());
    Some(spec)
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
    use super::{GearName, ProxyRegistry, ToolKitGatewayProvider, spawn_directory_sync, sync_once};
    use std::sync::Arc;
    use std::time::Duration;

    use cf_system_sdks::directory::{DirectoryClient, RegisterInstanceInfo, ServiceEndpoint};
    use tokio_util::sync::CancellationToken;
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
        assert_eq!(registry.instance_count(), 2);

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
    async fn sync_picks_up_spec_change_on_resync() {
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));

        // Initial spec exposes /calc/v1/add.
        let id = register(
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
        assert!(registry.match_path("/calc/v1/sub").is_none());

        // The same instance re-publishes a changed document: /add removed,
        // /sub added. Because each poll re-fetches and fully rebuilds, the next
        // sync reflects the new document exactly (not a stale union of both).
        dir.register_instance(RegisterInstanceInfo {
            gear: "calc".to_owned(),
            instance_id: id.clone(),
            grpc_services: vec![],
            version: None,
            rest_endpoint: Some(ServiceEndpoint::new("http://calc:8080")),
            openapi_spec: Some(public_spec("calc", "/calc/v1/sub")),
        })
        .await
        .unwrap();

        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert!(registry.match_path("/calc/v1/sub").is_some());
        assert!(registry.match_path("/calc/v1/add").is_none());
        assert_eq!(registry.instance_count(), 1);
    }

    #[tokio::test]
    async fn sync_retains_routes_when_new_instance_fails_but_gear_still_present() {
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
        assert!(registry.contains_instance(&GearName::from("calc"), &good));

        // The gear adds a second instance with an unparseable (schemeless) REST
        // endpoint, so that instance is skipped. The original valid instance must
        // remain active.
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
        assert!(registry.contains_instance(&GearName::from("calc"), &good));
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

        assert_eq!(registry.instance_count(), 0);
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

        assert_eq!(registry.instance_count(), 0);
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

    #[tokio::test]
    async fn sync_deregisters_only_one_instance_of_same_gear() {
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));

        let calc_a = register(
            dir.as_ref(),
            "calc",
            "http://calc-a:8080",
            public_spec("calc", "/calc/v1/add"),
        )
        .await;
        let calc_b = register(
            dir.as_ref(),
            "calc",
            "http://calc-b:8080",
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

        assert!(registry.contains_instance(&GearName::from("calc"), &calc_a));
        assert!(registry.contains_instance(&GearName::from("calc"), &calc_b));
        assert_eq!(registry.instance_count(), 3);

        // Profile 3 selection: the gear resolves to a single, stable endpoint;
        // cross-replica load balancing is delegated to the gear's k8s Service
        // DNS, not performed in the proxy.
        let matched = registry.match_path("/calc/v1/add").expect("calc match");
        assert_eq!(matched.gear.as_str(), "calc");
        assert!(matches!(
            matched.endpoint.authority().as_str(),
            "calc-a:8080" | "calc-b:8080"
        ));
        assert!(registry.match_path("/billing/v1/pay").is_some());

        // Remove one calc instance; the gear stays routable via the surviving
        // instance and billing is untouched.
        dir.deregister_instance("calc", &calc_a).await.unwrap();
        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert!(!registry.contains_instance(&GearName::from("calc"), &calc_a));
        assert!(registry.contains_instance(&GearName::from("calc"), &calc_b));
        assert!(registry.contains_gear(&GearName::from("calc")));
        assert_eq!(registry.instance_count(), 2);

        let remaining = registry
            .match_path("/calc/v1/add")
            .expect("calc still proxied");
        assert_eq!(remaining.endpoint.authority().as_str(), "calc-b:8080");
        assert!(registry.match_path("/billing/v1/pay").is_some());
    }

    #[tokio::test]
    async fn sync_skips_mass_deregistration_on_empty_snapshot() {
        // A successful-but-empty snapshot while instances are still registered
        // is treated as a transient directory hiccup: the poll must NOT prune
        // every route at once. The next non-empty poll reconciles normally.
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));

        let calc = register(
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
        assert_eq!(registry.instance_count(), 1);

        // The directory now reports an empty snapshot. The route table must be
        // retained rather than flushed.
        dir.deregister_instance("calc", &calc).await.unwrap();
        assert!(dir.list_all_instances().await.unwrap().is_empty());
        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert_eq!(registry.instance_count(), 1);
        assert!(registry.match_path("/calc/v1/add").is_some());
    }

    #[tokio::test]
    async fn sync_prunes_normally_when_snapshot_nonempty() {
        // The safety valve must not suppress legitimate removals: a non-empty
        // snapshot that no longer lists a previously registered instance still
        // prunes it.
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));

        let calc = register(
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
        assert_eq!(registry.instance_count(), 2);

        // calc disappears but billing remains: the snapshot is non-empty, so
        // calc is pruned as usual.
        dir.deregister_instance("calc", &calc).await.unwrap();
        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert!(!registry.contains_gear(&GearName::from("calc")));
        assert!(registry.match_path("/billing/v1/pay").is_some());
        assert_eq!(registry.instance_count(), 1);
    }

    #[tokio::test]
    async fn spawn_loop_syncs_then_stops_on_cancel() {
        // Exercises the background loop end-to-end: the first interval tick
        // fires immediately and reconciles the directory into the registry;
        // cancelling then drives the loop to a clean stop.
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));
        register(
            dir.as_ref(),
            "calc",
            "http://calc:8080",
            public_spec("calc", "/calc/v1/add"),
        )
        .await;

        let registry = Arc::new(ProxyRegistry::new());
        let cancel = CancellationToken::new();
        let handle = spawn_directory_sync(
            Arc::clone(&registry),
            Arc::clone(&dir),
            Duration::from_secs(1),
            cancel.clone(),
        );

        // The first tick is immediate; wait for the reconcile to land.
        let mut synced = false;
        for _ in 0..100 {
            if registry.match_path("/calc/v1/add").is_some() {
                synced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(synced, "background loop should reconcile on the first tick");

        cancel.cancel();
        handle.await.expect("sync loop joins cleanly after cancel");
    }
}
