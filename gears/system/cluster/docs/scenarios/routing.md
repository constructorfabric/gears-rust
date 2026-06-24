# Routing & SDK Defaults — Scenario Details (`SC-ROUTE-*`)

> Detail for §12 of the [scenario catalog](./README.md). Template: *Intent / Steps /
> Expected / Done-when*. These assert the "implement cache only, get all four primitives"
> guarantee at the SDK-default level. Operator-config auto-wrap and native-binding
> rejection are wiring-level ([SC-LIFE-006](./lifecycle.md)).
>
> **Ownership.** Unlike every other `SC-*` family, these two scenarios are **not**
> implemented in `cluster-conformance` — they live in the `cluster` gear crate's own
> test suite (`cluster/src/defaults/{leader,lock,discovery}_tests.rs`). Reason:
> `cluster-conformance` is a dependency of every backend plugin, so its real
> `[dependencies]` deliberately stay limited to `cluster-sdk`; it never depends on
> `cluster` (the wiring gear that implements `CasBasedLeaderElectionBackend` /
> `CasBasedDistributedLockBackend` / `CacheBasedServiceDiscoveryBackend`), even as a
> dev-dependency. `cluster` is the only crate that has both a bare cache fixture and
> the concrete defaults, so proving "cache-only in, all four primitives out" has to
> happen there.

**SC-ROUTE-001 — cache-only backend yields all four primitives** · L2 · ☑ (`cluster/src/defaults/{leader,lock,discovery}_tests.rs`)
- *Intent:* `fr-routing-cache-only-plugin` — a plugin that implements only `ClusterCacheBackend` must get working leader election, lock, and service discovery for free.
- *Steps:* wrap a cache-only backend (`MemoryCache`) in `CasBasedLeaderElectionBackend` / `CasBasedDistributedLockBackend` / `CacheBasedServiceDiscoveryBackend`, then exercise each primitive's contract directly.
- *Expected:* leader election (`single_candidate_becomes_leader`, `second_candidate_is_follower`, `foreign_takeover_emits_lost_then_resolves`, ...), lock (`try_lock_acquires_then_contends_while_held`, `release_frees_the_lock_for_a_new_acquirer`, ...), and discovery (`register_then_discover_finds_the_instance`, `watch_observes_join_then_leave`, ...) all behave correctly over the cache-derived defaults.
- *Done-when:* `cluster/src/defaults/leader_tests.rs`, `lock_tests.rs`, and `discovery_tests.rs` are green. These are bespoke assertions rather than a literal run of the shared `cluster-conformance` suite (see the ownership note above) — the property proven is the same claim, just not via the identical shared test body used for external backends.

**SC-ROUTE-002 — default features derive from the cache** · L2 · ☑ (`cluster/src/defaults/{leader,lock,discovery}_tests.rs`)
- *Intent:* a default backend must honestly inherit its guarantee from the cache it wraps — no over-claiming (ties into capability validation, §3.11).
- *Steps:* build the SDK defaults over (a) a `Linearizable` cache and (b) an `EventuallyConsistent` cache (via `new_allow_weak_consistency`); read `features()`.
- *Expected:* `LeaderElectionFeatures.linearizable` and `LockFeatures.linearizable` equal `cache.consistency() == Linearizable`; `CacheBasedServiceDiscoveryBackend` reports `metadata_pushdown == false`.
- *Done-when:* `leader_tests.rs::weak_consistency_constructor_always_succeeds_and_features_track_cache`, `lock_tests.rs::weak_consistency_constructor_succeeds_and_features_track_cache`, and `discovery_tests.rs::features_report_no_metadata_pushdown` are green.
