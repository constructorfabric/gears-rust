# cf-gears-cluster-conformance

Parametrized **backend conformance suites** for the cluster gear — the keystone
of [`docs/TESTING-STRATEGY.md`](../docs/TESTING-STRATEGY.md) §4 (Layer 2).

One shared test body, run against every backend, is the only mechanism that
operationalizes `cpt-cf-clst-nfr-cross-backend-stability`: "a consumer gear's
behavior MUST NOT change when an operator switches the backend." You can only
test "identical observable behavior" by running identical assertions everywhere.

## Using it from a plugin

A plugin's integration test reduces to ~10 lines: build the real backend (in a
container, for L3), hand it to the matching runner.

```rust
use std::sync::Arc;
use cluster_conformance::run_cache_conformance;
use cluster_sdk::ClusterCacheBackend;

#[tokio::test]
async fn my_backend_is_conformant() {
    run_cache_conformance(|| {
        // build the real backend; the runner calls this once per scenario so
        // state never leaks between cases.
        Arc::new(MyCacheBackend::connect(/* container endpoint */)) as Arc<dyn ClusterCacheBackend>
    })
    .await;
}
```

Cache-only plugins get the other three primitives for free by feeding their cache
through the `cluster` gear's `CasBased*` / `CacheBased*` defaults, then running
`run_leader_conformance` / `run_lock_conformance` / `run_discovery_conformance` over
the result — see the rustdoc on [`run_leader_conformance`](src/leader.rs). This crate
doesn't wire that composition itself (it deliberately has no dependency on `cluster`,
even for its own tests — see [routing.md](../docs/scenarios/routing.md)'s ownership
note); a plugin or the `cluster` gear does the wiring and calls in.

## Layout

| File | Contents |
|---|---|
| `fixture.rs` | `MemCache` — a compact linearizable in-process backend the suite self-tests against; reference fixture for the cache-derived suites |
| `cache.rs` | `SC-CACHE-*` scenarios + `run_cache_conformance` |
| `leader.rs` | `SC-LEAD-*` scenarios + `run_leader_conformance` |
| `lock.rs` | `SC-LOCK-*` scenarios + `run_lock_conformance` |
| `discovery.rs` | `SC-DISC-*` scenarios + `run_discovery_conformance` |
| `model.rs` | reference-model replay engine (`replay_against_model`) for CAS/version invariants; the `proptest` strategy driving it lives in `tests/model.rs` |

## Capability gating

Each suite reads the backend's `features()` / `consistency()` and asserts strict
guarantees **only** where the backend claims them (e.g. single-leader-under-
contention only when `linearizable == true`). A backend that lies about a
capability fails the suite — this is how `cpt-cf-clst-fr-validation-honest-
declaration` is tested.

## Coverage status

Each `scenario_*` maps to an `SC-*` row in the
[scenario catalog](../docs/scenarios/README.md). Catalog rows still marked ☐/◑
appear here as `// TODO(SC-*)` markers; L4 rows (fault injection, DST) are
delivered with their respective harnesses, not this in-process suite.
