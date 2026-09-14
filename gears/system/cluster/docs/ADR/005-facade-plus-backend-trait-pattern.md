---
status: accepted
date: 2026-04-27
---

# ADR-005: Per-primitive Facade + Backend Trait, No Root `Cluster` Trait, Per-primitive `*V1` Versioning

**ID**: `cpt-cf-clst-adr-facade-backend-pattern`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Why three sub-decisions in one ADR](#why-three-sub-decisions-in-one-adr)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Single root `Cluster` / `ClusterV1` trait with all three primitives](#option-1-single-root-cluster--clusterv1-trait-with-all-three-primitives)
  - [Option 2: Trait with associated types for each primitive](#option-2-trait-with-associated-types-for-each-primitive)
  - [Option 3: Per-primitive trait, single trait surface (`ClusterCacheV1: Trait`)](#option-3-per-primitive-trait-single-trait-surface-clustercachev1-trait)
  - [Option 4: Per-primitive facade struct + per-primitive backend trait + per-primitive `*V1` versioning (CHOSEN)](#option-4-per-primitive-facade-struct--per-primitive-backend-trait--per-primitive-v1-versioning-chosen)
  - [Option 5: Trait alias on nightly to bundle re-exports](#option-5-trait-alias-on-nightly-to-bundle-re-exports)
  - [Option 6: Gear-path versioning (`v1::ClusterCache`)](#option-6-gear-path-versioning-v1clustercache)
  - [Option 7: Gear-major-version-as-crate-name](#option-7-gear-major-version-as-crate-name)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The cluster gear exposes three coordination primitives — distributed cache, leader election, distributed lock — to every Gear. The shape of those three primitives in the Rust type system is the most consequential structural decision in the SDK: it determines what consumers hold, what plugins implement, how operator config maps to code, and what evolution looks like over time.

Three sub-decisions are unavoidable and tightly coupled:

1. **Is there a root `Cluster` (or `ClusterV1`) trait that bundles all three primitives, or three independent surfaces?** A root trait reads naturally as "the cluster" but forces every plugin to implement all three primitives even when one of them is a poor fit (e.g., a pure cache plugin must stub out leader election). It also couples the three primitives' versioning lifecycles — bumping any primitive bumps `Cluster` itself.

2. **What do consumers hold, and what do plugins implement?** A single trait that both consumers depend on (`&dyn ClusterCache`) and plugins implement is convenient but pushes consumers onto the `dyn` surface for every method call, makes ergonomic improvements (inherent methods, type-state, builders) impossible without breaking plugin authors, and conflates two audiences with different needs.

3. **How does each primitive evolve incompatibly?** If the cache primitive needs a breaking change but leader election does not, can it ship a `*V2` without forcing every plugin to migrate three primitives at once? The mechanism (separate `TypeKey` registration in ClientHub, per-crate file split, etc.) determines whether incompatible evolution is cheap or expensive.

The facade-plus-backend-trait pattern with per-primitive versioning resolves all three. This ADR captures why these three sub-decisions are inseparable and why the alternatives — many of them ergonomic at first glance — fail under realistic evolution pressure.

## Decision Drivers

- **Plugin minimum effort**: a plugin should be able to ship one primitive (cache only) without a stub-out chore for the other three. SDK default backends (per ADR-001) make a minimal cache-only plugin sufficient for the common case.
- **Consumer ergonomics**: consumers should hold inherent-method types they can clone cheaply, not `&dyn Trait` references requiring `tokio::spawn` arc-cloning gymnastics.
- **Independent versioning**: each primitive must evolve incompatibly without forcing the other three to bump. A breaking change to `ClusterCache` should not force `LeaderElection` plugins to migrate.
- **No magic strings or runtime registry coupling**: ClientHub is a typed registry. Whatever the public-API and plugin-facing types are, they must register and resolve via Rust's type system, not via string identifiers.
- **Coexisting versions**: when `*V2` ships, `*V1` consumers must keep working unchanged. The two versions must register side-by-side under different type keys.
- **Stable plugin contract surface**: consumers shouldn't churn when plugins evolve internally; plugin authors shouldn't churn when consumer ergonomics improve. Two surfaces, two evolution lifecycles.

## Considered Options

1. **Single root `Cluster` / `ClusterV1` trait with all three primitives** — one trait, plugins implement all three methods.
2. **Trait with associated types for each primitive** — `trait Cluster { type Cache: ...; type Leader: ...; ... }`.
3. **Per-primitive trait, single trait surface** — `trait ClusterCacheV1 { ... }` is both what consumers depend on AND what plugins implement.
4. **Per-primitive facade struct + per-primitive backend trait + per-primitive `*V1` versioning** — `ClusterCacheV1` (facade struct) wraps `Arc<dyn ClusterCacheBackend>`; consumers hold the facade, plugins impl the backend trait. (CHOSEN.)
5. **Trait alias on nightly to bundle re-exports** — `trait ClusterV1 = ClusterCacheV1 + LeaderElectionV1 + ...`
6. **Gear-path versioning** — `v1::ClusterCache`, `v2::ClusterCache` co-exist via gear paths.
7. **Gear-major-version-as-crate-name** — `cf-cluster-sdk-1`, `cf-cluster-sdk-2` shipped as separate crates.

## Decision Outcome

Chosen option: **Option 4** — per-primitive facade struct + per-primitive backend trait, with per-primitive `*V1` versioning via separate ClientHub `TypeKey` registration.

Concretely, the SDK exposes three pairs:

| Public-API facade (consumers hold) | Plugin-facing backend trait (plugins implement) |
|---|---|
| `ClusterCacheV1` (struct, cheap-clone, `Arc`-backed) | `ClusterCacheBackend` (`#[async_trait]`, dyn-compatible) |
| `LeaderElectionV1` | `LeaderElectionBackend` |
| `DistributedLockV1` | `DistributedLockBackend` |

Each `*V1` is a struct with inherent async methods (`get`, `put`, `compare_and_swap`, `watch`, etc. on `ClusterCacheV1`) and inherent sync methods (`consistency()`, `features()`, `resolver(hub)`, `scoped(prefix)`). Internally it wraps `Arc<dyn _Backend>`. Cloning the facade is a single atomic increment.

Plugins implement only the backend trait their plugin actually serves. A pure cache plugin implements `ClusterCacheBackend`; the SDK's omit-primitive auto-wrap (per ADR-001 / DESIGN §3.11) builds the other two from the cache backend.

There is **no** root `Cluster` or `ClusterV1` trait. There is no facade struct that bundles "the cluster". Each primitive is registered independently in ClientHub under a per-primitive `TypeKey`, scoped by profile.

Per-primitive versioning: when the cache primitive needs an incompatible change, the SDK ships `ClusterCacheV2` + `ClusterCacheBackendV2` alongside `*V1`. Both register in ClientHub under separate type keys. Consumers migrate at their own pace; plugins ship `V2` support when ready. Leader election and lock are unaffected.

### Why three sub-decisions in one ADR

The three sub-decisions are inseparable:

- Rejecting the root trait *is* the per-primitive versioning argument. If you keep a root `Cluster` trait, you can't bump cache without bumping the root trait, which forces every plugin to acknowledge the bump.
- Splitting facade from backend trait *is* the consumer-ergonomics argument. If consumers depend on `dyn Trait`, you can't add inherent methods or change the resolver shape without breaking plugin authors who implement that same trait.
- The two together *enable* per-primitive `*V1` typing. Per-primitive backend traits register independently in ClientHub; per-primitive facades type-check the resolver result; the per-primitive nature flows through the entire SDK.

You cannot adopt one of the three and reject the others without losing the property the third was solving for. Hence one ADR.

### Consequences

- **Plugin authors** implement only what they serve. A cache-only plugin is one trait impl plus a builder/handle. The SDK fills in the rest.
- **Consumers** hold cheap-clone facades. `ClusterCacheV1` is `Clone + Send + Sync` and goes wherever the consumer needs it without `Arc<Mutex<...>>` ceremony.
- **Inherent methods** on the facade let the SDK improve ergonomics (typed resolvers, scoping helpers, `Builder`-style configuration) without breaking plugin authors. The plugin contract (the backend trait) is small and stable; the consumer contract (the facade) can grow features.
- **Per-primitive versioning** is a normal release operation. Adding `ClusterCacheV2` is a non-event for `LeaderElectionV1` plugins. The SDK ships a `V1` ↔ `V2` adapter when there's a path; otherwise consumers migrate on their own schedule.
- **Two evolution lifecycles** (facade for consumers, backend trait for plugins) require discipline: every change must be classified as "consumer-facing" or "plugin-facing" or "both". This is the right friction — it forces clarity about what's actually breaking.
- **No bundled `Cluster` accessor** means consumers wanting all three primitives resolve three times. The fluent resolver (per ADR-007) makes each resolution a one-liner; ergonomics are not the bottleneck.
- **ClientHub registration is per-primitive per profile**, not per-cluster. The wiring crate iterates the profile × primitive matrix and registers each `Arc<dyn _Backend>` independently. This is what makes mixed-backend profiles (Redis cache + K8s Lease elections) trivial.
- **Dyn-compatibility** of the backend traits is enforced by compile-time assertions per trait. Any future change that breaks dyn-compatibility (e.g., adding a generic method, returning `impl Trait`) fails the build.
- **Resolution is `async`; the facades are not.** `resolve()` is `async fn` on all three resolvers, because validating a declared capability against a *remote* binding needs a `ProfileDescriptor` that must be fetched, and a synchronous signature cannot await one (DESIGN §3.10.1). This is the **only** signature the remote-backend model changes — the facades themselves, `scoped()`, the watch-event unions and `auto_restart` are untouched, which is what keeps one consumer source file valid in both an embedded and a deployed cluster. The cost to a consumer is one `.await` in `start`, already an `async fn`. In-process resolution has nothing to await and validates inline, so the async-ness is a contract for the remote case rather than a change in local behaviour.

### Confirmation

- Compile-time `_assert_dyn_compat(_: Arc<dyn _Backend>) {}` per backend trait. Build fails if dyn-compatibility breaks.
- A consumer mock test holds `ClusterCacheV1` in a struct field and clones it across tasks; the test passes only if `ClusterCacheV1: Clone + Send + Sync + 'static`.
- A plugin author test implements only `ClusterCacheBackend` and resolves all three primitives from the SDK defaults — passing demonstrates the cache-only-plugin path works end-to-end.
- A future-version test stubs `ClusterCacheV2` registered side-by-side with `V1` under different type keys; both resolve without conflict.

## Pros and Cons of the Options

### Option 1: Single root `Cluster` / `ClusterV1` trait with all three primitives

```rust
trait Cluster {
    fn cache(&self) -> &dyn ClusterCache;
    fn leader_election(&self) -> &dyn LeaderElection;
    fn distributed_lock(&self) -> &dyn DistributedLock;
}
```

- Good, because consumers reference "the cluster" as a single object — feels natural for "this gear needs cluster".
- Good, because the consumer-side type is one type, not three.
- Bad, because every plugin implements all three primitives or panics. A cache-only plugin must stub `leader_election` and `distributed_lock` with "unsupported" returns — every consumer of those stubs hits runtime errors instead of startup-time validation.
- Bad, because per-primitive versioning is impossible. Bumping `ClusterCache` forces `Cluster` to bump. Forcing `Cluster` to bump forces every plugin to acknowledge — even plugins that don't ship the cache primitive.
- Bad, because the three primitives are not actually one bundled service. They have different consistency requirements, different backends, different lifecycle characteristics, different operator-side configuration. Bundling them in one trait is structural lying.
- Bad, because mixed-backend profiles (Redis cache + K8s Lease elections) become awkward — there's no single object that owns both backends; you'd need a `HybridCluster` impl that routes per primitive, which is exactly the runtime compositor we explicitly rejected.

### Option 2: Trait with associated types for each primitive

```rust
trait Cluster {
    type Cache: ClusterCacheBackend;
    type Leader: LeaderElectionBackend;
    type Lock: DistributedLockBackend;
    fn cache(&self) -> &Self::Cache;
    // ...
}
```

- Good, because associated types push the primitive concrete types into the impl, not the trait.
- Bad, because `dyn Cluster` is impossible — associated types make the trait non-dyn-compatible. Consumers must be generic over the cluster impl, which infects every cluster-using function with type parameters.
- Bad, because per-primitive versioning still forces the root trait to bump (the associated types change shape).
- Bad, because mixing backends in one cluster impl is now harder — the impl must name its concrete cache/leader/lock types statically. A wiring impl that wires Redis-cache + K8s-leader has to spell out the concrete types.

### Option 3: Per-primitive trait, single trait surface (`ClusterCacheV1: Trait`)

```rust
trait ClusterCacheV1 {
    async fn get(&self, key: &str) -> Result<...>;
    async fn put(&self, key: &str, ...) -> Result<()>;
    fn consistency(&self) -> CacheConsistency;
    fn resolver(hub: &ClientHub) -> CacheResolverBuilder<'_>;
    // ...
}
```

- Good, because per-primitive — no bundling. Plugins implement only what they serve.
- Good, because per-primitive versioning works (`ClusterCacheV1` and `ClusterCacheV2` are two traits).
- Bad, because consumers depend on `&dyn ClusterCacheV1` for every method call — they can't hold an inherent-method type. The `dyn` surface is the consumer-facing API.
- Bad, because adding ergonomic methods to the consumer-facing API (a fluent resolver, a `scoped` helper) requires changing the trait, which breaks plugin authors. The two audiences (consumers, plugin authors) share one trait surface.
- Bad, because static methods on the trait (`fn resolver(hub: &ClientHub)`) clash with dyn-compatibility. You either scatter the resolver as a free function or compromise dyn-compat.
- Bad, because every method call goes through a vtable indirection that the compiler can't inline — minor perf overhead at the hottest path. With facade + backend trait, the facade methods can be `#[inline]`'d to call into the backend.

### Option 4: Per-primitive facade struct + per-primitive backend trait + per-primitive `*V1` versioning (CHOSEN)

```rust
pub struct ClusterCacheV1 {
    inner: Arc<dyn ClusterCacheBackend>,
}

impl ClusterCacheV1 {
    pub fn resolver(hub: &ClientHub) -> CacheResolverBuilder<'_> { ... }
    pub async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError> {
        self.inner.get(key).await
    }
    pub fn scoped(&self, prefix: &str) -> ClusterCacheV1 { ... }
    // ...
}

#[async_trait]
pub trait ClusterCacheBackend: Send + Sync + 'static {
    async fn get(&self, key: &str) -> Result<Option<CacheEntry>, ClusterError>;
    // ...
}
```

- Good, because consumers hold an inherent-method struct: cheap to clone, easy to put in struct fields, no `dyn` ceremony.
- Good, because plugins implement only the backend trait — small, stable, dyn-compatible.
- Good, because the facade can grow ergonomic methods (resolver, scoping, builders) without changing the plugin-facing trait.
- Good, because per-primitive versioning is straightforward: `ClusterCacheV2` + `ClusterCacheBackendV2` ship alongside `V1`. ClientHub registration uses different type keys; both versions co-exist.
- Good, because static methods (`resolver`) are inherent on the facade — no dyn-compat compromise on the backend trait.
- Good, because dyn-compatibility is asserted at compile time per backend trait; any change breaking dyn-compat fails the build.
- Good, because hot-path methods on the facade can inline-delegate to the backend, avoiding extra function-call overhead.
- Bad, because consumers wanting all three primitives resolve three times. Mitigated by the fluent resolver (one-liner per primitive — see ADR-007).
- Bad, because the SDK maintains both a facade and a backend trait per primitive — six types instead of three. Worth it for the two evolution lifecycles.
- Neutral, because the consumer-visible naming convention (`ClusterCacheV1` for the consumer surface, `ClusterCacheBackend` for plugins) is an explicit convention contributors must learn. Doc comments and consistent naming across the three primitives make it discoverable.

### Option 5: Trait alias on nightly to bundle re-exports

```rust
#![feature(trait_alias)]
trait ClusterV1 = ClusterCacheV1 + LeaderElectionV1 + DistributedLockV1;
```

- Good, because consumers can write `&dyn ClusterV1` if they want all three primitives.
- Bad, because `trait_alias` is unstable on nightly indefinitely — the platform pins MSRV 1.92.0 stable.
- Bad, because trait aliases compose by trait inheritance; plugins implementing the alias must implement all three constituent traits — same problem as Option 1.
- Bad, because adoption requires nightly toolchain everywhere, which is a non-starter.

### Option 6: Gear-path versioning (`v1::ClusterCache`)

```rust
pub mod v1 { pub trait ClusterCache { ... } }
pub mod v2 { pub trait ClusterCache { ... } }
```

- Good, because the version is visible in the path.
- Bad, because the *type identity* is what matters for ClientHub registration. `v1::ClusterCache` and `v2::ClusterCache` already have different `TypeId`s by virtue of being different types — the gear path doesn't add anything that wouldn't already work with `ClusterCacheV1` / `ClusterCacheV2` at the crate root.
- Bad, because `use cluster_sdk::v1::ClusterCache as ClusterCache;` is awkward at every consumer site. Putting the version in the type name (`ClusterCacheV1`) reads more naturally and matches Rust ecosystem convention (e.g., `tower-service`, `http`, gRPC stubs).
- Neutral, because gear-path versioning and `*V1` naming are equivalent in capability; we choose the latter for ergonomics.

### Option 7: Gear-major-version-as-crate-name

`cf-cluster-sdk-1`, `cf-cluster-sdk-2` shipped as separate crates.

- Good, because crate-level versioning is the cleanest possible isolation.
- Good, because Cargo's semver model handles co-existing versions natively.
- Bad, because a major version bump becomes a *new crate*, not a new release. Doc URLs change, dependency declarations churn, every consumer must add the new crate to `Cargo.toml`.
- Bad, because shared code between versions (resolver, error types, common helpers) requires a third "core" crate that both versioned crates depend on, multiplying maintenance.
- Bad, because the cluster gear is consumed by every Gear — forcing all of them to update their crate declaration on a major version is a heavyweight migration. `*V1` / `*V2` types in one crate is lighter.
- Neutral, because this approach is established in some ecosystems (e.g., `axum` 0.6 / 0.7 differences); we choose against it because cluster's audience is internal gears, not external consumers, where lightweight migration matters more than crate-level isolation.

## More Information

**Why "per-primitive versioning" is concretely possible.** ClientHub uses `TypeId` to key its registry. `ClusterCacheBackend` and `ClusterCacheBackendV2` are different traits → different `TypeId`s → independent registration slots. A wiring crate that wants to support both versions registers the same plugin's backend impl twice (once per version) or registers only the version it implements. Consumers depending on `ClusterCacheV1` resolve via `V1`'s type key; consumers depending on `V2` resolve via `V2`'s key. Both can coexist in the same ClientHub, in the same profile, simultaneously.

**Why facade methods don't need to be `dyn`-compatible.** The facade is a concrete struct, not a trait. Inherent methods can use generics, return `impl Trait`, take type-state parameters, etc. Only the backend trait — what plugins implement — needs to be dyn-compatible.

**Why the backend trait is `#[async_trait]` and not native `async fn`.** Native `async fn` in traits exists on stable but produces non-dyn-compatible signatures by default. We need dyn-compat for ClientHub registration. `#[async_trait]` rewrites async fn into `Pin<Box<dyn Future>>`-returning sync fn, which is dyn-compatible. The boxed-future allocation per call is acceptable overhead at the plugin boundary; hot paths inside the plugin avoid the box by calling concrete impls directly.

**Why `Arc<dyn _Backend>` and not `Box<dyn _Backend>`.** The facade is `Clone`. `Arc` lets cloning the facade be a single atomic increment. `Box` would force consumers to wrap the facade in `Arc` themselves, which reintroduces the `Arc<...>` ceremony at every consumer site.

**References:**

- ADR-001 — backend compatibility and the cache-CAS-universal model. The SDK-default backend implementations are what makes "cache-only plugin is sufficient" workable.
- ADR-006 — builder/handle lifecycle. Plugins are nested builder/handle pairs whose handle owns the plugin's `Arc<dyn _Backend>` registered in ClientHub.
- ADR-007 — capability typing and typed profile resolution. The fluent resolver returns the per-primitive facade, completing the consumer-side ergonomic story.
- DESIGN.md §1.1 (architectural vision), §2.1 (`facade-plus-backend-trait` principle), §3.1 (domain model with six types: three facades + three backend traits), §3.2 (component model showing per-primitive ClientHub registration).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-clst-fr-routing-per-primitive` — Per-primitive routing as operator config is enabled by per-primitive facade + backend trait.
- `cpt-cf-clst-nfr-cross-backend-stability` — Plugin contract surface stays stable as consumer ergonomics evolve.
- `cpt-cf-clst-nfr-plugin-stability` — Per-primitive `*V1` versioning lets plugins evolve incompatibly without coupled bumps.
- `cpt-cf-clst-principle-facade-plus-backend-trait` (DESIGN §2.1) — Stated as a load-bearing design principle.
- `cpt-cf-clst-component-sdk` (DESIGN §3.2) — SDK hosts the three facade structs + three backend traits.
- `cpt-cf-clst-component-wiring` (DESIGN §3.2) — Wiring registers `Arc<dyn _Backend>` per profile per primitive.
- `cpt-cf-clst-component-plugins` (DESIGN §3.2) — Plugins implement only the backend traits they serve.
- DESIGN §3.1 Domain Model — Six type entries (three `*V1` facades + three `*Backend` traits) are the realization of this ADR.

**Sibling ADRs:**

- ADR-001 — Cache CAS as universal primitive enables the SDK-default-backend strategy that builds on this pattern.
- ADR-006 — Plugins follow builder/handle pair shape per this ADR's per-primitive backend trait surface.
- ADR-007 — Per-primitive capability typing is enabled by the per-primitive facade.
