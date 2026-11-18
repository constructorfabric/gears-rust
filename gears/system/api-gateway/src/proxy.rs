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

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use cf_system_sdks::directory::{DirectoryClient, InstanceState, ServiceInstanceInfo};
use tokio_util::sync::CancellationToken;
use toolkit_gateway::{
    Endpoint, GatewayProvider, GearInstance, GearName, InstanceSpec, OpenApiSpec,
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
///
/// Driven entirely through the [`GatewayProvider`] trait, so the loop is
/// agnostic to the concrete edge (the built-in reverse proxy, or an external
/// Kong/Tyk adapter): pass whichever provider the deployment profile selected.
pub fn spawn_directory_sync(
    provider: Arc<dyn GatewayProvider>,
    directory: Arc<dyn DirectoryClient>,
    interval: Duration,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(directory_sync_loop(provider, directory, interval, cancel))
}

/// Poll the directory on a fixed cadence, reconciling the proxy route table,
/// until `cancel` fires.
async fn directory_sync_loop(
    provider: Arc<dyn GatewayProvider>,
    directory: Arc<dyn DirectoryClient>,
    interval: Duration,
    cancel: CancellationToken,
) {
    let mut ticker = tokio::time::interval(interval.max(MIN_SYNC_INTERVAL));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Persisted across ticks so an unchanged snapshot can be skipped.
    let mut state = SyncState::default();

    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = ticker.tick() => {
                if reconcile_pass(provider.as_ref(), directory.as_ref(), &mut state, &cancel)
                    .await
                    .is_break()
                {
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
    provider: &dyn GatewayProvider,
    directory: &dyn DirectoryClient,
    state: &mut SyncState,
    cancel: &CancellationToken,
) -> std::ops::ControlFlow<()> {
    tokio::select! {
        () = cancel.cancelled() => std::ops::ControlFlow::Break(()),
        result = tokio::time::timeout(SYNC_TIMEOUT, reconcile(provider, directory, state)) => {
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
async fn reconcile(
    provider: &dyn GatewayProvider,
    directory: &dyn DirectoryClient,
    state: &mut SyncState,
) {
    if let Err(err) = sync_once_stateful(provider, directory, state).await {
        tracing::warn!(error = %err, "gateway directory-sync poll failed");
    }
}

/// Poll the directory once and reconcile it into the route table, using `state`
/// to skip the fetch + rebuild when the snapshot is unchanged. Errors only if
/// the directory poll itself fails; per-gear problems are logged and skipped.
async fn sync_once_stateful(
    provider: &dyn GatewayProvider,
    directory: &dyn DirectoryClient,
    state: &mut SyncState,
) -> anyhow::Result<()> {
    let instances = directory.list_all_instances().await?;
    reconcile_snapshot(provider, directory, instances, state).await;
    Ok(())
}

/// One-shot reconcile with a throwaway [`SyncState`], so it always performs a
/// full pass. Test-only convenience over [`sync_once_stateful`].
#[cfg(test)]
async fn sync_once(
    provider: &dyn GatewayProvider,
    directory: &dyn DirectoryClient,
) -> anyhow::Result<()> {
    sync_once_stateful(provider, directory, &mut SyncState::default()).await
}

/// How many consecutive empty directory snapshots the empty-snapshot valve
/// preserves existing routes across before giving up and pruning. Tolerates a
/// brief directory hiccup without pinning routes to crashed instances forever
/// (at the default 10s poll cadence this is a ~30s grace window).
const MAX_EMPTY_POLLS_PRESERVING_ROUTES: u32 = 3;

/// Whether the edge can route to an instance: it advertises a REST endpoint and
/// a published spec. gRPC-only instances are not routable here — the edge
/// proxies HTTP — so they neither become routes nor decide serving/fallback.
fn is_proxyable(instance: &ServiceInstanceInfo) -> bool {
    instance.rest_endpoint.is_some() && instance.openapi_spec_hash.is_some()
}

/// Lets each poll skip the spec re-fetch and router rebuild when the snapshot
/// is unchanged — a k8s `resourceVersion` / xDS `version_info` analogue.
#[derive(Default)]
struct SyncState {
    /// Fingerprint of the last snapshot reconciled *in full*. `None` forces the
    /// next poll to reconcile (startup, a partial pass, or the empty-snapshot
    /// safety valve), so pending work is always retried.
    last_applied: Option<u64>,
    /// Consecutive polls where the directory returned an empty snapshot, used to
    /// bound the empty-snapshot valve (see [`MAX_EMPTY_POLLS_PRESERVING_ROUTES`]).
    consecutive_empty_polls: u32,
}

/// Prefer serving instances per gear for edge routing.
///
/// If a gear has any serving (`Ready`/`Healthy`) instance, keep only its
/// serving instances; otherwise keep its not-yet-serving instances, so a gear
/// still coming up is not dropped from the edge (the same fallback as directory
/// name round-robin). A `Draining` instance is never kept — draining means
/// "stop routing new work," so a gear whose only instances are draining leaves
/// the edge entirely rather than serving new requests a `503` drain response.
/// Instances past their lease grace window are already absent from the snapshot.
///
/// "Serving" is decided only among [`is_proxyable`] instances: the edge routes
/// HTTP, so a serving gRPC-only replica carries no route that could stand in for
/// a starting REST replica of the same gear. Counting it would drop that REST
/// replica and leave the gear with no usable HTTP endpoint.
fn prefer_serving_per_gear(instances: Vec<ServiceInstanceInfo>) -> Vec<ServiceInstanceInfo> {
    let gears_with_serving: HashSet<String> = instances
        .iter()
        .filter(|i| is_proxyable(i) && i.state.is_serving())
        .map(|i| i.gear.clone())
        .collect();
    instances
        .into_iter()
        // A draining instance is shutting down: never route new work to it, even
        // as a fallback, so a draining-only gear is dropped from the route table.
        .filter(|i| i.state != InstanceState::Draining)
        .filter(|i| i.state.is_serving() || !gears_with_serving.contains(&i.gear))
        .collect()
}

/// Order-independent fingerprint over the proxyable instances (those with a
/// REST endpoint and a spec hash): gear, instance id, authority, and spec hash
/// — every input the route table depends on.
fn snapshot_fingerprint(instances: &[ServiceInstanceInfo]) -> u64 {
    let mut entries: Vec<(&str, &str, &str, &str)> = instances
        .iter()
        .filter_map(|i| {
            let rest = i.rest_endpoint.as_ref()?;
            let hash = i.openapi_spec_hash.as_deref()?;
            Some((
                i.gear.as_str(),
                i.instance_id.as_str(),
                rest.uri.as_str(),
                hash,
            ))
        })
        .collect();
    entries.sort_unstable();
    let mut hasher = DefaultHasher::new();
    entries.hash(&mut hasher);
    hasher.finish()
}

/// Reconcile a whole directory snapshot into the route table in one batched
/// apply (one router rebuild per poll), skipping the pass entirely when the
/// snapshot is unchanged since the last full apply.
async fn reconcile_snapshot(
    provider: &dyn GatewayProvider,
    directory: &dyn DirectoryClient,
    instances: Vec<ServiceInstanceInfo>,
    state: &mut SyncState,
) {
    // Whether the directory itself returned nothing, captured before the
    // draining filter below can empty the set. An empty *directory* snapshot is
    // a transient hiccup (keep the routes); a snapshot emptied only because
    // every instance is draining must still prune (withdraw those routes).
    let directory_reported_nothing = instances.is_empty();

    // Bound the empty-snapshot valve (see `MAX_EMPTY_POLLS_PRESERVING_ROUTES`):
    // preserve routes across a few empty polls, then let the prune run.
    let preserve_empty_routes = if directory_reported_nothing {
        state.consecutive_empty_polls = state.consecutive_empty_polls.saturating_add(1);
        state.consecutive_empty_polls <= MAX_EMPTY_POLLS_PRESERVING_ROUTES
    } else {
        state.consecutive_empty_polls = 0;
        false
    };

    // Route to serving instances only, per gear: the edge no longer
    // proxies to a non-serving instance when a serving one exists. Instances
    // whose lease expired past grace are already absent from the snapshot.
    let instances = prefer_serving_per_gear(instances);

    // Fast path: if the proxyable snapshot is byte-for-byte the same as the one
    // we last applied in full, there is nothing to fetch or rebuild.
    let fingerprint = snapshot_fingerprint(&instances);
    if state.last_applied == Some(fingerprint) {
        tracing::debug!(
            fingerprint,
            "directory snapshot unchanged since last apply; skipping spec fetch and route rebuild",
        );
        return;
    }

    let Resolved {
        desired,
        to_register,
        complete,
    } = resolve_snapshot(directory, instances).await;

    let (removals, empty_snapshot_valve) = compute_removals(
        provider.registered_instances(),
        &desired,
        preserve_empty_routes,
    );

    provider.apply_snapshot(to_register, &removals).await;

    // Cache the fingerprint only when the applied state fully matches the
    // snapshot; otherwise clear it so the next poll retries the pending work.
    state.last_applied = if complete && !empty_snapshot_valve {
        Some(fingerprint)
    } else {
        None
    };
}

/// Outcome of resolving a directory snapshot into registrable routes.
struct Resolved {
    /// Every (gear, `instance_id`) in the snapshot — the prune baseline, so a
    /// still-present instance whose spec fetch merely failed is not dropped.
    desired: HashSet<GearInstance>,
    /// Instances whose routes were successfully extracted.
    to_register: Vec<InstanceSpec<'static>>,
    /// Whether every proxyable instance resolved; when `false` the fingerprint
    /// is not cached, so the next poll retries.
    complete: bool,
}

/// Resolve every instance in a snapshot into routes, fetching each gear's spec
/// at most once per pass (`Bytes` is ref-counted, so co-located instances of a
/// gear share one copy).
async fn resolve_snapshot(
    directory: &dyn DirectoryClient,
    instances: Vec<ServiceInstanceInfo>,
) -> Resolved {
    let mut desired: HashSet<GearInstance> = HashSet::new();
    let mut specs: HashMap<GearName, Bytes> = HashMap::new();
    let mut to_register: Vec<InstanceSpec<'static>> = Vec::with_capacity(instances.len());
    let mut complete = true;

    for inst in instances {
        let gear = GearName::from(inst.gear.as_str());
        desired.insert(GearInstance::new(gear.clone(), &inst.instance_id));

        let proxyable = is_proxyable(&inst);
        match resolve_instance_spec(directory, &inst, &gear, &mut specs).await {
            Some(spec) => to_register.push(spec),
            None if proxyable => complete = false,
            None => {}
        }
    }

    Resolved {
        desired,
        to_register,
        complete,
    }
}

/// Which registered instances to deregister given the desired set, plus whether
/// the empty-snapshot safety valve fired: a *directory* snapshot that is empty
/// while instances are still registered is treated as a transient hiccup (skip
/// the prune this poll) rather than every gear vanishing at once.
///
/// `preserve_empty_routes` (set by [`reconcile_snapshot`]) means the directory
/// returned nothing and is still within [`MAX_EMPTY_POLLS_PRESERVING_ROUTES`].
/// It is deliberately not keyed off `desired.is_empty()`: a `desired` set
/// emptied because every instance is draining is a deliberate exclusion, so it
/// must still prune and withdraw those draining routes.
fn compute_removals(
    registered: Vec<GearInstance>,
    desired: &HashSet<GearInstance>,
    preserve_empty_routes: bool,
) -> (Vec<GearInstance>, bool) {
    if preserve_empty_routes && !registered.is_empty() {
        tracing::warn!(
            registered = registered.len(),
            "directory snapshot is empty while instances are registered; \
             skipping deregistration this poll to avoid mass route withdrawal",
        );
        return (Vec::new(), true);
    }
    let removals = registered
        .into_iter()
        .filter(|instance| !desired.contains(instance))
        .collect();
    (removals, false)
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
    use super::{
        MAX_EMPTY_POLLS_PRESERVING_ROUTES, SyncState, prefer_serving_per_gear,
        snapshot_fingerprint, spawn_directory_sync, sync_once, sync_once_stateful,
    };
    use cf_system_sdks::directory::InstanceState;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use toolkit_gateway::{
        Endpoint, GatewayError, GatewayProvider, GearInstance, GearName, InstanceSpec, OpenApiSpec,
        ProxyRegistry, ToolKitGatewayProvider,
    };

    use cf_system_sdks::directory::{
        DirectoryClient, RegisterInstanceInfo, ReportedState, ServiceEndpoint, ServiceInstanceInfo,
    };
    use tokio_util::sync::CancellationToken;
    use toolkit::directory::LocalDirectoryClient;
    use toolkit::runtime::GearManager;

    /// A REST-proxyable instance (a REST endpoint and a published spec hash),
    /// as the directory reports for a REST gear — the only kind the edge routes,
    /// and so the only kind that decides serving vs fallback.
    fn instance(gear: &str, id: &str, state: InstanceState) -> ServiceInstanceInfo {
        ServiceInstanceInfo::new(gear, id)
            .with_state(state)
            .with_rest_endpoint(Some(ServiceEndpoint::new(format!(
                "http://{gear}-{id}:8080"
            ))))
            .with_openapi_spec_hash(Some(format!("hash-{gear}")))
    }

    /// A gRPC-only instance (no REST endpoint, no spec hash): the edge cannot
    /// route it, so it must not influence REST serving/fallback selection.
    fn grpc_only_instance(gear: &str, id: &str, state: InstanceState) -> ServiceInstanceInfo {
        ServiceInstanceInfo::new(gear, id).with_state(state)
    }

    #[test]
    fn prefer_serving_keeps_only_serving_when_any_serves() {
        let picked = prefer_serving_per_gear(vec![
            instance("billing", "a", InstanceState::Healthy),
            instance("billing", "b", InstanceState::Draining),
            instance("billing", "c", InstanceState::Quarantined),
        ]);
        let ids: Vec<_> = picked.iter().map(|i| i.instance_id.as_str()).collect();
        assert_eq!(ids, ["a"], "only the serving instance is routed to");
    }

    #[test]
    fn prefer_serving_falls_back_to_all_when_none_serve() {
        let mut picked = prefer_serving_per_gear(vec![
            instance("billing", "a", InstanceState::Registered),
            instance("billing", "b", InstanceState::Quarantined),
        ]);
        picked.sort_by(|x, y| x.instance_id.cmp(&y.instance_id));
        let ids: Vec<_> = picked.iter().map(|i| i.instance_id.as_str()).collect();
        assert_eq!(
            ids,
            ["a", "b"],
            "with no serving instance, every visible instance is kept"
        );
    }

    #[test]
    fn prefer_serving_is_per_gear() {
        // One gear serving, another with none serving: each decided on its own.
        let picked = prefer_serving_per_gear(vec![
            instance("billing", "a", InstanceState::Healthy),
            instance("billing", "b", InstanceState::Registered),
            instance("catalog", "c", InstanceState::Registered),
        ]);
        let mut kept: Vec<_> = picked.iter().map(|i| i.instance_id.as_str()).collect();
        kept.sort_unstable();
        assert_eq!(
            kept,
            ["a", "c"],
            "serving-only for billing, fallback for catalog"
        );
    }

    #[test]
    fn prefer_serving_ignores_a_grpc_only_serving_replica() {
        // A serving gRPC-only replica carries no HTTP route, so it must not mark
        // its gear as "serving" and suppress a starting REST replica — otherwise
        // the gear would lose its only usable REST endpoint from the edge.
        let picked = prefer_serving_per_gear(vec![
            grpc_only_instance("billing", "grpc", InstanceState::Healthy),
            instance("billing", "rest", InstanceState::Registered),
        ]);
        let ids: Vec<_> = picked.iter().map(|i| i.instance_id.as_str()).collect();
        assert!(
            ids.contains(&"rest"),
            "a starting REST replica must survive a serving gRPC-only replica, got {ids:?}"
        );
    }

    #[test]
    fn prefer_serving_drops_draining_even_as_fallback() {
        // A gear whose only instance is draining leaves the edge entirely: a
        // draining instance must never be a routing target, not even a fallback
        // (otherwise a gracefully-stopping gear keeps its route and answers new
        // requests a 503 drain response instead of the route being withdrawn).
        let picked =
            prefer_serving_per_gear(vec![instance("billing", "a", InstanceState::Draining)]);
        assert!(
            picked.is_empty(),
            "a draining-only gear must be dropped from the edge, got {picked:?}"
        );

        // Draining is dropped while a co-located not-yet-serving instance stays.
        let picked = prefer_serving_per_gear(vec![
            instance("billing", "a", InstanceState::Draining),
            instance("billing", "b", InstanceState::Registered),
        ]);
        let ids: Vec<_> = picked.iter().map(|i| i.instance_id.as_str()).collect();
        assert_eq!(ids, ["b"], "draining dropped, starting kept");
    }

    /// Wraps a [`ToolKitGatewayProvider`] and counts how many times
    /// [`GatewayProvider::apply_snapshot`] actually runs, so tests can assert
    /// the sync loop skips the rebuild when the snapshot is unchanged.
    struct CountingProvider {
        inner: ToolKitGatewayProvider,
        applies: Arc<AtomicUsize>,
    }

    impl CountingProvider {
        fn new(registry: Arc<ProxyRegistry>) -> (Self, Arc<AtomicUsize>) {
            let applies = Arc::new(AtomicUsize::new(0));
            let provider = Self {
                inner: ToolKitGatewayProvider::new(registry),
                applies: Arc::clone(&applies),
            };
            (provider, applies)
        }
    }

    #[async_trait::async_trait]
    impl GatewayProvider for CountingProvider {
        async fn register_routes(
            &self,
            gear: &GearName,
            instance_id: &str,
            spec: OpenApiSpec<'_>,
            endpoint: &Endpoint,
        ) -> Result<(), GatewayError> {
            self.inner
                .register_routes(gear, instance_id, spec, endpoint)
                .await
        }

        async fn deregister_routes(
            &self,
            gear: &GearName,
            instance_id: &str,
        ) -> Result<(), GatewayError> {
            self.inner.deregister_routes(gear, instance_id).await
        }

        fn registered_instances(&self) -> Vec<GearInstance> {
            self.inner.registered_instances()
        }

        async fn apply_snapshot<'a>(
            &self,
            instances: Vec<InstanceSpec<'a>>,
            removals: &[GearInstance],
        ) {
            self.applies.fetch_add(1, Ordering::SeqCst);
            self.inner.apply_snapshot(instances, removals).await;
        }
    }

    fn public_spec(gear: &str, path: &str) -> String {
        serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": gear, "version": "1.0.0" },
            "paths": { path: { "get": { "x-toolkit-visibility": "exposed", "responses": {} } } },
        })
        .to_string()
    }

    async fn register(dir: &dyn DirectoryClient, gear: &str, uri: &str, spec: String) -> String {
        let instance_id = uuid::Uuid::new_v4().to_string();
        dir.register_instance(
            RegisterInstanceInfo::new(gear, instance_id.clone())
                .with_rest_endpoint(ServiceEndpoint::new(uri))
                .with_openapi_spec(spec),
        )
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
        dir.register_instance(
            RegisterInstanceInfo::new("calc", id.clone())
                .with_rest_endpoint(ServiceEndpoint::new("http://calc:8080"))
                .with_openapi_spec(public_spec("calc", "/calc/v1/sub")),
        )
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
        dir.register_instance(
            RegisterInstanceInfo::new("worker", uuid::Uuid::new_v4().to_string())
                .with_grpc_services(vec![(
                    "worker.Svc".to_owned(),
                    ServiceEndpoint::new("http://worker:7000"),
                )]),
        )
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
        dir.register_instance(
            RegisterInstanceInfo::new("specless", uuid::Uuid::new_v4().to_string())
                .with_rest_endpoint(ServiceEndpoint::new("http://specless:8080")),
        )
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

        // Precondition: the snapshot is spec-less — it carries only the hash,
        // never the inlined document.
        let snapshot = dir.list_all_instances().await.unwrap();
        assert!(snapshot.iter().all(|i| i.openapi_spec_hash.is_some()));

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
    async fn sync_prunes_dead_routes_after_repeated_empty_snapshots() {
        // The empty-snapshot valve tolerates a brief hiccup, but a directory
        // that stays empty (the last instance crashed and its lease was swept)
        // must eventually prune — otherwise the edge proxies a dead endpoint
        // forever. Uses a persistent `SyncState` so the empty-poll count carries
        // across polls as it does in the real loop.
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
        let mut state = SyncState::default();
        sync_once_stateful(&provider, dir.as_ref(), &mut state)
            .await
            .unwrap();
        assert_eq!(registry.instance_count(), 1);

        // The instance disappears; the directory reports empty from now on.
        dir.deregister_instance("calc", &calc).await.unwrap();
        assert!(dir.list_all_instances().await.unwrap().is_empty());

        // Within the bound the valve preserves the route across empty polls.
        for _ in 0..MAX_EMPTY_POLLS_PRESERVING_ROUTES {
            sync_once_stateful(&provider, dir.as_ref(), &mut state)
                .await
                .unwrap();
            assert_eq!(
                registry.instance_count(),
                1,
                "the route is preserved while within the empty-poll bound",
            );
        }

        // One empty poll past the bound finally withdraws the dead route.
        sync_once_stateful(&provider, dir.as_ref(), &mut state)
            .await
            .unwrap();
        assert_eq!(
            registry.instance_count(),
            0,
            "a persistently empty directory must eventually withdraw dead routes",
        );
        assert!(registry.match_path("/calc/v1/add").is_none());
    }

    #[tokio::test]
    async fn sync_withdraws_routes_when_all_instances_are_draining() {
        // A non-empty directory snapshot whose only instance is draining must
        // withdraw the route. The draining filter empties the desired set, but
        // the directory did report an instance, so the empty-snapshot safety
        // valve must NOT fire and preserve a route to a draining instance.
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
        assert_eq!(registry.instance_count(), 1);

        // The only instance flips to Draining (still observable in the snapshot).
        dir.heartbeat("calc", &calc, Some(ReportedState::Draining))
            .await
            .unwrap();
        sync_once(&provider, dir.as_ref()).await.unwrap();

        assert_eq!(
            registry.instance_count(),
            0,
            "a draining-only snapshot must withdraw its routes, not preserve them via the empty-snapshot valve",
        );
        assert!(registry.match_path("/calc/v1/add").is_none());
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
        let provider: Arc<dyn GatewayProvider> =
            Arc::new(ToolKitGatewayProvider::new(Arc::clone(&registry)));
        let cancel = CancellationToken::new();
        let handle = spawn_directory_sync(
            provider,
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

    #[tokio::test]
    async fn sync_skips_rebuild_when_snapshot_unchanged() {
        // Two consecutive polls of an unchanged directory: the first reconciles
        // (apply_snapshot runs); the second must be skipped via the spec-hash
        // fingerprint — no re-fetch, no router rebuild — while routes remain.
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
        let (provider, applies) = CountingProvider::new(Arc::clone(&registry));
        let mut state = SyncState::default();

        sync_once_stateful(&provider, dir.as_ref(), &mut state)
            .await
            .unwrap();
        assert_eq!(applies.load(Ordering::SeqCst), 1, "first poll reconciles");
        assert!(registry.match_path("/calc/v1/add").is_some());

        sync_once_stateful(&provider, dir.as_ref(), &mut state)
            .await
            .unwrap();
        assert_eq!(
            applies.load(Ordering::SeqCst),
            1,
            "unchanged second poll is skipped (no rebuild)"
        );
        assert!(registry.match_path("/calc/v1/add").is_some());
    }

    #[tokio::test]
    async fn sync_reapplies_after_spec_change() {
        // A spec change flips the directory's content hash, so the fingerprint
        // differs and the next poll reconciles rather than skipping.
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));
        let id = register(
            dir.as_ref(),
            "calc",
            "http://calc:8080",
            public_spec("calc", "/calc/v1/add"),
        )
        .await;

        let registry = Arc::new(ProxyRegistry::new());
        let (provider, applies) = CountingProvider::new(Arc::clone(&registry));
        let mut state = SyncState::default();

        sync_once_stateful(&provider, dir.as_ref(), &mut state)
            .await
            .unwrap();
        assert_eq!(applies.load(Ordering::SeqCst), 1);

        // Re-publish a changed document for the same instance.
        dir.register_instance(
            RegisterInstanceInfo::new("calc", id)
                .with_rest_endpoint(ServiceEndpoint::new("http://calc:8080"))
                .with_openapi_spec(public_spec("calc", "/calc/v1/sub")),
        )
        .await
        .unwrap();

        sync_once_stateful(&provider, dir.as_ref(), &mut state)
            .await
            .unwrap();
        assert_eq!(
            applies.load(Ordering::SeqCst),
            2,
            "changed spec hash forces a reconcile"
        );
        assert!(registry.match_path("/calc/v1/sub").is_some());
        assert!(registry.match_path("/calc/v1/add").is_none());
    }

    #[tokio::test]
    async fn sync_retries_when_snapshot_unchanged_but_last_pass_incomplete() {
        // If a proxyable instance fails to resolve (here: an unparseable REST
        // endpoint), the fingerprint must NOT be cached, so an unchanged next
        // poll still reconciles rather than skipping the never-applied instance.
        let mgr = Arc::new(GearManager::new());
        let dir: Arc<dyn DirectoryClient> = Arc::new(LocalDirectoryClient::new(Arc::clone(&mgr)));
        register(
            dir.as_ref(),
            "calc",
            "/schemeless-uri",
            public_spec("calc", "/calc/v1/add"),
        )
        .await;

        let registry = Arc::new(ProxyRegistry::new());
        let (provider, applies) = CountingProvider::new(Arc::clone(&registry));
        let mut state = SyncState::default();

        sync_once_stateful(&provider, dir.as_ref(), &mut state)
            .await
            .unwrap();
        assert_eq!(applies.load(Ordering::SeqCst), 1);
        assert!(
            state.last_applied.is_none(),
            "incomplete pass is not cached"
        );

        sync_once_stateful(&provider, dir.as_ref(), &mut state)
            .await
            .unwrap();
        assert_eq!(
            applies.load(Ordering::SeqCst),
            2,
            "unresolved proxyable instance forces a retry next poll"
        );
    }

    fn instance_with_hash(gear: &str, uri: &str, hash: Option<&str>) -> ServiceInstanceInfo {
        ServiceInstanceInfo::new(gear, "inst-1")
            .with_endpoint(Some(ServiceEndpoint::new(uri)))
            .with_rest_endpoint(Some(ServiceEndpoint::new(uri)))
            .with_openapi_spec_hash(hash.map(ToOwned::to_owned))
    }

    #[test]
    fn snapshot_fingerprint_is_stable_and_change_sensitive() {
        let a = vec![instance_with_hash("calc", "http://calc:8080", Some("h1"))];
        let a_again = vec![instance_with_hash("calc", "http://calc:8080", Some("h1"))];
        let changed = vec![instance_with_hash("calc", "http://calc:8080", Some("h2"))];

        assert_eq!(
            snapshot_fingerprint(&a),
            snapshot_fingerprint(&a_again),
            "identical snapshots fingerprint equally"
        );
        assert_ne!(
            snapshot_fingerprint(&a),
            snapshot_fingerprint(&changed),
            "a changed spec hash changes the fingerprint"
        );
    }
}
