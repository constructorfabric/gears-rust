# Cluster wiring

`cf-gears-cluster` (lib `cluster`) is the wiring crate for the cluster gear
(DESIGN §3.4 / §3.7, component `cpt-cf-clst-component-wiring`). It registers the
per-profile, per-primitive coordination backends produced by cluster plugins
into the `ClientHub` — under the `cluster:{profile}` scope the SDK resolvers look
them up in — and owns the cluster lifecycle.

The crate plays two roles (DESIGN §3.7, as amended): it **is** the `cluster`
gear — a ToolKit `RunnableCapability` whose `start` builds the wiring from
operator config (`ClusterConfig` / `ClusterWiring::from_config`) and whose
`stop` tears it down — and it also exposes the underlying outbox-style
builder/handle pair as a `pub` library, so a consumer gear may embed the wiring
directly instead of depending on the `cluster` gear. Either way, a single
`RunnableCapability` owns the resulting `ClusterHandle` from its own
`start`/`stop`:

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

// Consumers resolve the three primitives for `EventBroker` via the SDK resolvers.

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

## Linking this gear requires `grpc-hub`

The gear declares the `grpc` capability and serves the four coordination services
(`cluster.{cache,lock,leader,profile}.v1`), so **any process that links `cluster`
must also link `grpc-hub` and give it a `listen_addr`** — including an in-process
monolith. Without it, startup fails with `RegistryError::GrpcRequiresHub`.

Plan for two things when embedding: cluster's services become reachable on that
process's hub port, so it needs the same `NetworkPolicy` treatment as a dedicated
cluster pod; and the hub has to bind a port you are willing to expose. See
`docs/DESIGN.md` §3.15.1.

## Running as a deployable gear — `cluster-oop`

This crate also ships a binary, so cluster can run as its own pod instead of
being linked into a consumer (DESIGN §3.15, Profile 3):

```bash
cargo build -p cf-gears-cluster --bin cluster-oop
cluster-oop --config /etc/cluster/config.yaml
```

`src/main.rs` is a `clap` CLI over `toolkit::bootstrap::oop::run_oop_with_options`
and contains nothing else; `src/registered_gears.rs` names the two gears the
process links (`cluster` and `grpc_hub` — see the section above). Everything a pod
needs is the bootstrap's: the HTTP listener with `/healthz`, `/readyz`, `/health`
and `/openapi.json` served **before** the gear's `start` runs, self-registration
with backoff, the presence loop, the drain sequence and DirectoryService
deregistration on SIGTERM. **None of it is in this crate**, which ADR-0005
requires and `tests/oop_bootstrap.rs` asserts.

Two things the config must carry, both of which are hard failures rather than
degradations:

- an **`oop_http`** section with a `listen_addr`. Without it the bootstrap takes
  its legacy gRPC-only path and serves no HTTP — no probes at all.
- a **`gears.grpc-hub.config.listen_addr`**, for the reason in the section above.

The process also needs a reachable **DirectoryService**: the bootstrap connects to
it eagerly and propagates the failure, so `cluster-oop` does not start without
one. The endpoint comes from `TOOLKIT_DIRECTORY_ENDPOINT` (default
`http://127.0.0.1:50051`).

## Status

Backends can be wired two ways: programmatically via `ProfileBackends`
(shown above), or from operator YAML via `ClusterConfig` and
`ClusterWiring::from_config`, which parses each profile's per-primitive
`provider` bindings, dispatches them against a `ProviderRegistry` of linked
plugins, and lets the omit-default auto-wrap supply any primitive left
unbound. The `cluster` gear (`gear.rs`) uses the YAML-driven path; the
programmatic path remains available for a consumer gear that wants to embed
the wiring directly. Today the only linked cache provider is the standalone
in-process plugin (`cf-gears-standalone-cluster-plugin`); additional backend
plugins (Postgres, Redis, K8s, NATS, etcd) are follow-up changes.

## License

Apache-2.0
