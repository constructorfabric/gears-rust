# Cluster wiring

`cf-gears-cluster` (lib `cluster`) is the wiring crate for the cluster gear
(DESIGN §3.4 / §3.7, component `cpt-cf-clst-component-wiring`). It registers the
per-profile, per-primitive coordination backends produced by cluster plugins
into the `ClientHub` — under the `cluster:{profile}` scope the SDK resolvers look
them up in — and owns the cluster lifecycle.

It is **not** a ToolKit `RunnableCapability`. Following the outbox-style
builder/handle pattern, a parent host gear owns the `ClusterHandle` from its own
`RunnableCapability::start`/`stop`:

```rust,no_run
use cluster::{ClusterWiring, ProfileBackends};
use cluster_sdk::ClusterProfile;

struct EventBroker;
impl ClusterProfile for EventBroker {
    const NAME: &'static str = "event-broker";
}

# async fn run(
#     hub: std::sync::Arc<toolkit::client_hub::ClientHub>,
#     cache: std::sync::Arc<dyn cluster_sdk::ClusterCacheBackend>,
# ) -> Result<(), cluster_sdk::ClusterError> {
let handle = ClusterWiring::builder(hub)
    .profile(EventBroker, ProfileBackends::new(cache)) // omit-default: cache only
    .build_and_start()?;

// Consumers resolve the four primitives for `EventBroker` via the SDK resolvers.

handle.stop().await; // deregisters all backends, then stops wired plugins
# Ok(())
# }
```

## Routing

- **Per-primitive** — a profile may bind a different backend per primitive
  (`ProfileBackends::new(cache).with_lock(..).with_leader_election(..)`),
  realizing `cpt-cf-clst-fr-routing-per-primitive`.
- **Omit-default** — any primitive left unbound is auto-filled with the SDK
  default backend over the profile's cache
  (`cpt-cf-clst-fr-routing-omit-default`).

## Lifecycle

`build_and_start` resolves all backends first (so a failure cannot leave a
partially-registered hub), then registers them. `ClusterHandle::stop`
deregisters every backend and runs the registered plugin shutdown hooks; no
best-effort remote cleanup is attempted — TTL bounds remaining cluster resources
(`cpt-cf-clst-fr-shutdown-ttl-cleanup`).

## Status

The scaffold wires backends supplied programmatically via `ProfileBackends`.
Parsing operator YAML into per-profile/per-primitive backend selection and
instantiating the matching plugins (DESIGN's `ClusterConfig`) is the follow-up
layer that will feed these same bindings.

## License

Apache-2.0
