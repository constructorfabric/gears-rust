# Standalone cluster plugin

`cf-gears-standalone-cluster-plugin` (lib `standalone_cluster_plugin`) is the
in-process, zero-infrastructure backend for the cluster gear — the default for
developer laptops and unit/integration tests (PRD §3.1, DESIGN §3.11).

It implements the **cache** primitive natively over an in-memory store
(`HashMap` + monotonic per-key versions, lazy TTL, a background sweeper emitting
`Expired`, and exact/prefix watches) and derives **leader election**,
**distributed lock**, and **service discovery** from the SDK's cache-based
default backends — the "implement cache only, get all four" guarantee. The cache
declares `Linearizable` consistency, which the default leader/lock backends
require.

## Lifecycle

Not a ToolKit `RunnableCapability`. It follows the outbox-style builder/handle
pattern (DESIGN §3.7) owned by a parent host gear or the cluster wiring crate:

```rust,no_run
use standalone_cluster_plugin::StandaloneClusterPlugin;

# async fn run() -> Result<(), cluster_sdk::ClusterError> {
let handle = StandaloneClusterPlugin::builder().build_and_start()?;

// Hand the backends to the wiring crate / register them in the ClientHub:
let cache     = handle.cache();
let leader    = handle.leader_election();
let lock      = handle.lock();
let discovery = handle.service_discovery();

// On graceful shutdown:
handle.stop().await;
# Ok(())
# }
```

`build_and_start` starts the cache TTL sweeper; `handle.stop()` cancels it. The
plugin performs no remote I/O and never relies on `Drop` for cleanup; cluster
resources are bounded by their TTL.

## Status

Cache is native. Leader election, lock, and service discovery currently ride the
SDK default backends over the native cache. Native implementations of those three
(the DESIGN §3.11 end state) are a follow-up optimization.

## License

Apache-2.0
