---
status: accepted
date: 2026-04-27
---

# ADR-007: Per-primitive Capability Typing and Typed Profile Resolution

**ID**: `cpt-cf-clst-adr-capability-typing-and-profile-resolution`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Per-primitive `*Capability` enums](#per-primitive-capability-enums)
  - [Typed `ClusterProfile` marker](#typed-clusterprofile-marker)
  - [Fluent resolver as the natural consequence](#fluent-resolver-as-the-natural-consequence)
  - [Capability-mismatch fails startup](#capability-mismatch-fails-startup)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Bundled CapabilityClass enum](#option-1-bundled-capabilityclass-enum)
  - [Option 2: Struct-of-options requirements](#option-2-struct-of-options-requirements)
  - [Option 3: No validation — rely on plugin opt-in only](#option-3-no-validation--rely-on-plugin-opt-in-only)
  - [Option 4: String profile names](#option-4-string-profile-names)
  - [Option 5: Per-primitive `*Capability` enums + typed `ClusterProfile` marker (CHOSEN)](#option-5-per-primitive-capability-enums--typed-clusterprofile-marker-chosen)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

A consumer gear needs to declare what cluster guarantees it requires, and the SDK needs to verify those guarantees against the operator-bound backend at the right time. "The right time" is **startup, not runtime**. A consumer that requires a linearizable cache and is bound to an eventually-consistent Redis-Sentinel deployment must fail loudly at gear boot, not silently corrupt state under partition six months later.

This requires three things:

1. **A vocabulary for declaring requirements.** What can the consumer say it needs?
2. **A binding mechanism for selecting the right backend.** How does the consumer name which operator-configured profile to use?
3. **A validation moment that fails loudly.** When does the SDK check requirements against the backend's actual characteristics, and how does it surface mismatches?

An earlier iteration of the design used a single bundled `CapabilityClass { Standalone, Durable, InMemory, Coordination }` enum to express requirements across all three primitives at once. This collapsed three orthogonal axes — topology (single-node vs multi-node), persistence (volatile vs durable), and consistency (eventual vs linearizable) — into one fuzzy ordering. A `Coordination`-class consumer "obviously needs more than `Durable`," but is `Durable` weaker than or stronger than `InMemory`? The bundling forced false comparisons across axes that should be evaluated independently, and a primitive-agnostic enum could not express "this primitive needs linearizable, but that one is fine with eventually consistent."

For profile binding, an obvious choice is `String` profile names. The cluster picks up `profile: "event-broker"` and resolves accordingly. This works mechanically but spreads profile strings across every consumer call site, makes refactor-renames a sprawling find-and-replace, and provides no compile-time guarantee that two crates using the same profile spell it the same way.

Both problems share a root cause: under-typed APIs leak structural decisions into stringly-typed runtime values. The fix is to push both into the type system: per-primitive `*Capability` enums grounded in concrete backend characteristic checks, and a typed `ClusterProfile` marker trait so the profile string lives in exactly one place per consumer crate.

## Decision Drivers

- **Type-safe requirements**: a cache resolver should not accept `MetadataFiltering`. The compiler should reject it.
- **Concrete characteristic checks**: every variant of every `*Capability` enum should map to an observable check against the backend's `consistency()` or `features()`. No fuzzy "tier" comparisons.
- **Independent axes**: topology, persistence, and consistency are three different things, and a primitive may need any combination. Per-primitive enums let each primitive express its own axis.
- **Single-source profile names**: a profile string should appear once per consumer crate, not at every resolver call site.
- **Startup-time failure**: a misconfigured backend must fail the gear's `RunnableCapability::start()` with a clear, actionable error. Runtime failures hours after start are unacceptable.
- **Composable with the facade pattern**: the resolver must produce per-primitive `*V1` facades (per ADR-005), not a bundled `Cluster` object.

## Considered Options

1. **Bundled `CapabilityClass { Standalone, Durable, InMemory, Coordination }`** — one enum across all three primitives.
2. **Struct-of-options requirements** — `Requirements { linearizable: bool, prefix_watch: bool, ... }` per primitive.
3. **No validation** — declare nothing; rely on plugin opt-in to advertise their characteristics.
4. **String profile names** — profile binding via `String`, no marker trait.
5. **Per-primitive `*Capability` enums + typed `ClusterProfile` marker** (CHOSEN) — each primitive has its own `*Capability` enum; consumer crates impl `ClusterProfile` on a ZST per profile.

## Decision Outcome

Chosen option: **Option 5** — per-primitive `*Capability` enums (one enum per primitive, each variant grounded in a concrete backend characteristic check) plus a typed `ClusterProfile` marker trait so profile names live in one place per consumer crate. The fluent resolver and capability-mismatch-fails-startup behavior are direct consequences.

### Per-primitive `*Capability` enums

Each primitive defines its own `#[non_exhaustive]` capability enum:

```rust
#[non_exhaustive]
pub enum CacheCapability {
    Linearizable,
    PrefixWatch,
}

#[non_exhaustive]
pub enum LeaderElectionCapability {
    Linearizable,
}

#[non_exhaustive]
pub enum LockCapability {
    Linearizable,
}
```

Each variant maps to a concrete characteristic check:

| Capability | Backend method | Check |
|---|---|---|
| `CacheCapability::Linearizable` | `ClusterCacheBackend::consistency()` | `== CacheConsistency::Linearizable` |
| `CacheCapability::PrefixWatch` | `ClusterCacheBackend::features()` | `.prefix_watch == true` |
| `LeaderElectionCapability::Linearizable` | `LeaderElectionBackend::features()` | `.linearizable == true` |
| `LockCapability::Linearizable` | `DistributedLockBackend::features()` | `.linearizable == true` |

`#[non_exhaustive]` on every enum lets the SDK add new capabilities without breaking consumers. Consumers that match on capability values must include a wildcard arm; in practice, consumers always pass a literal variant, never match against one.

### Typed `ClusterProfile` marker

The profile binding is via a marker trait:

```rust
pub trait ClusterProfile: 'static + Send + Sync + Copy {
    const NAME: &'static str;
}
```

Consumer crates implement the trait on a zero-sized type once per profile they consume:

```rust
#[derive(Clone, Copy)]
pub struct EventBrokerProfile;

impl ClusterProfile for EventBrokerProfile {
    const NAME: &'static str = "event-broker";
}
```

That string `"event-broker"` is the only place the profile name lives in the consumer crate. Resolver call sites pass `EventBrokerProfile` as a value (a copy of a ZST — zero runtime cost):

```rust
let cache = ClusterCacheV1::resolver(&hub)
    .profile(EventBrokerProfile)
    .require(CacheCapability::Linearizable)
    .require(CacheCapability::PrefixWatch)
    .resolve()?;
```

Refactoring "event-broker" to "broker" is one edit (the `const NAME`); every call site is unaffected.

### Fluent resolver as the natural consequence

Per-primitive resolution requires a per-primitive entry point. Inherent on each `*V1` facade:

```rust
impl ClusterCacheV1 {
    pub fn resolver(hub: &ClientHub) -> CacheResolverBuilder<'_>;
}
```

The builder accumulates state (chosen profile, required capabilities) and `.resolve()` performs the lookup-and-validate:

```rust
pub struct CacheResolverBuilder<'a> {
    hub: &'a ClientHub,
    profile_name: Option<&'static str>,
    requirements: Vec<CacheCapability>,
}

impl<'a> CacheResolverBuilder<'a> {
    pub fn profile<P: ClusterProfile>(mut self, _: P) -> Self {
        self.profile_name = Some(P::NAME);
        self
    }
    pub fn require(mut self, cap: CacheCapability) -> Self {
        self.requirements.push(cap);
        self
    }
    pub fn resolve(self) -> Result<ClusterCacheV1, ClusterError> {
        let profile = self.profile_name.ok_or(ClusterError::ProfileNotSpecified)?;
        let inner: Arc<dyn ClusterCacheBackend> = self.hub
            .get_scoped(profile_scope(profile))
            .map_err(|_| ClusterError::ProfileNotBound { profile })?;
        validate_cache_capabilities(&*inner, &self.requirements)?;
        Ok(ClusterCacheV1 { inner })
    }
}
```

Equivalent builders exist for `LeaderElectionV1` and `DistributedLockV1`. Each returns its own per-primitive facade, takes its own per-primitive `*Capability` enum, and validates against its own per-primitive characteristic check. A cache resolver builder will not even type-check if you call `.require(LockCapability::Linearizable)` — wrong enum.

### Capability-mismatch fails startup

`validate_*_capabilities` checks each requirement against the backend's declared characteristics. On mismatch, it returns `ClusterError::CapabilityNotMet { primitive, capability, provider }` where `provider` is `std::any::type_name_of_val(backend)` so operators see "the bound `RedisClusterCacheBackend` does not declare `Linearizable` consistency" rather than a generic message.

The resolver call lives in the consumer's `RunnableCapability::start()` (or in a constructor invoked from there). Failure propagates as `Result<_, ClusterError>` → `anyhow::Error` → the framework's `start()` failure path. The gear fails to start; the operator sees a clear error in logs identifying which consumer, which primitive, which capability, and which bound backend. Production traffic never sees the misconfiguration.

The error error-naming is deliberately precise — `ProfileNotSpecified` (you forgot to call `.profile(...)`), `ProfileNotBound { profile }` (operator config doesn't bind this profile), `CapabilityNotMet { primitive, capability, provider }` (backend exists but doesn't satisfy the requirement). Three distinct error states for three distinct misconfigurations.

### Consequences

- **A cache resolver cannot accept a lock capability.** The compiler rejects `CacheResolverBuilder::require(LockCapability::Linearizable)` because the builder's `require` takes `CacheCapability`. Type errors at the call site are unmistakable.
- **Profile names live in one place.** Refactoring a profile name is one `const NAME` edit. No find-and-replace across the consumer crate.
- **Compile-time profile typing.** Two crates that both consume profile `"event-broker"` either share a `ClusterProfile` ZST (in a shared crate) or each defines its own. The latter is fine — they happen to name the same string. No cross-crate string mismatch is possible because the resolver matches on `P::NAME` only, not on the ZST identity.
- **Capability validation is one function call per requirement.** The validation logic is per-primitive but trivial: match the capability variant, check the backend method, return `Err` if mismatched. No reflection, no schema introspection.
- **Adding a new capability is non-breaking.** `#[non_exhaustive]` lets the SDK add `CacheCapability::SecondaryIndex` (hypothetical) without forcing existing consumers to recompile. New plugins declare support in `features()`; old plugins that don't declare it just don't satisfy the new capability when consumers require it.
- **`profile_scope(name)` is the SDK's only stringly-typed surface for profiles.** It's an internal helper that maps `P::NAME` to a `ClientScope` for ClientHub registration/lookup. Consumers never see it directly.
- **No `NotStarted` error variant exists.** Pre-resolution access surfaces as `ProfileNotBound` from the resolver, not as `NotStarted` from a partially-constructed facade. Resolved facades cannot observe a "not started" state — the resolver enforces presence at consumer construction time.

### Confirmation

- A compile-fail test attempts `ClusterCacheV1::resolver(&hub).require(LockCapability::Linearizable)` and verifies it fails to compile (wrong capability type for the cache builder).
- A unit test resolves with no `.profile(...)` call and asserts `Err(ClusterError::ProfileNotSpecified)`.
- A unit test resolves a profile that has no binding registered in ClientHub and asserts `Err(ClusterError::ProfileNotBound { profile: "event-broker" })`.
- An integration test against a stub `ClusterCacheBackend` declaring `consistency() == EventuallyConsistent` resolves with `.require(CacheCapability::Linearizable)` and asserts `Err(ClusterError::CapabilityNotMet { primitive: "cache", capability: "Linearizable", provider: "MemCacheBackend" })`.
- A consumer-side smoke test uses two `ClusterProfile` ZSTs (`EventBrokerProfile`, `OagwProfile`) registered against different backends and resolves each independently — verifies the typed profile binding works end-to-end.

## Pros and Cons of the Options

### Option 1: Bundled CapabilityClass enum

- Good, because one enum is fewer types to learn.
- Bad, because it collapses three orthogonal axes into one fuzzy ordering. `Coordination` is "stronger than" `Durable` for what reason? `InMemory` excludes `Durable` — but does it exclude `Coordination`? Each comparison requires a paragraph of explanation.
- Bad, because a primitive-agnostic enum cannot express per-primitive requirements. A consumer that needs a linearizable lock but is fine with an eventually-consistent cache cannot say so.
- Bad, because adding new requirements means adding new variants to the same enum, mixing axes further. After a year, the enum has 12 variants and operators have to memorize the partial ordering between them.
- Bad, because the validation logic is forced into a `match`-and-priority-comparison shape that is hard to maintain and harder to test exhaustively.
- Neutral, because some consumers don't care about the distinctions and would happily use a single class — but those consumers can use Option 5's per-primitive enums with zero requirements (`.resolve()` without any `.require(...)`) and get the same simplicity.

### Option 2: Struct-of-options requirements

```rust
pub struct CacheRequirements {
    pub linearizable: bool,
    pub prefix_watch: bool,
}

resolver.require(CacheRequirements { linearizable: true, ..Default::default() })
```

- Good, because struct fields name the requirements.
- Good, because adding a new requirement is a non-breaking field addition (with `#[non_exhaustive]` on the struct).
- Bad, because all-bool struct fields encourage `..Default::default()` shorthand that makes the call site less specific — readers have to know the defaults to understand what the consumer actually requires.
- Bad, because mutually exclusive requirements (none currently exist, but possible — e.g., `causal_consistency: bool` vs `linearizable: bool`) are not compile-time enforced.
- Bad, because the natural construction `CacheRequirements { linearizable: true, prefix_watch: true }` is more verbose than `.require(Linearizable).require(PrefixWatch)`.
- Neutral, because both shapes (enum + builder method, or struct + single setter) produce equivalent compile-time guarantees. We choose the enum-and-builder shape for readability of call sites.

### Option 3: No validation — rely on plugin opt-in only

Plugins declare their characteristics; consumers don't declare requirements. The wiring crate logs the bound characteristics at startup; operators are responsible for matching them to consumer needs.

- Good, because zero validation infrastructure.
- Bad, because misconfiguration becomes a runtime failure or, worse, silent corruption. A `Linearizable`-needing consumer bound to `EventuallyConsistent` redis works most of the time and fails under partition.
- Bad, because operators carry the mental burden of cross-referencing every consumer's documented requirements against their backend choice. Cluster consumes nine gears; the cross-reference is a maintenance nightmare.
- Bad, because requirements drift silently as consumers evolve. A consumer that adds prefix-watch usage gets no signal that its previously-fine backend now lacks `PrefixWatch`.

### Option 4: String profile names

```rust
ClusterCacheV1::resolver(&hub).profile("event-broker").require(...)
```

- Good, because zero ceremony — no marker trait to define.
- Bad, because `"event-broker"` lives at every call site. Refactoring is N edits, with the risk of missing one.
- Bad, because typos are silent runtime failures. `"event-broker"` vs `"event-borker"` compiles fine and fails at resolution time with `ProfileNotBound`.
- Bad, because cross-crate consistency relies on convention, not the type system. Two crates may consume "the same profile" with subtly different spellings.
- Neutral, because the marker-trait shape (Option 5) is one trait impl per profile per crate. The ceremony is small; the gain is meaningful.

### Option 5: Per-primitive `*Capability` enums + typed `ClusterProfile` marker (CHOSEN)

- Good, because per-primitive types make wrong-axis errors compile failures (a cache resolver cannot accept a lock capability).
- Good, because each capability variant maps to a concrete backend characteristic check — no fuzzy tiering.
- Good, because `#[non_exhaustive]` lets the SDK add capabilities without breaking consumers.
- Good, because the typed profile marker keeps profile strings to one place per consumer crate.
- Good, because the fluent builder reads naturally at the call site (`.profile(P).require(Cap).resolve()`) and produces actionable error messages on mismatch.
- Good, because the resolver shape composes cleanly with the per-primitive facade pattern (per ADR-005) — each builder returns the matching `*V1`.
- Bad, because three `*Capability` enums and three resolver builders are more types than one bundled enum and one resolver. Worth it for the type-safety guarantees.
- Bad, because consumer crates must define a `ClusterProfile` ZST per profile they use. One impl per profile per consumer crate — a small ceremony, paid once per consumer.
- Neutral, because the trade-off (more types, stronger guarantees) is the same shape we chose in ADR-005 (more types per primitive, two evolution lifecycles). Consistent design philosophy.

## More Information

**Why `ClusterProfile` is `Copy + 'static`.** The resolver takes `P` by value. ZSTs are trivially `Copy`. `'static` keeps the resolver's lifetime story simple — no reference into a profile struct. Consumer call sites pass a literal value (`EventBrokerProfile`), and the resolver extracts `P::NAME` as a `&'static str`.

**Why `NAME` is `&'static str`, not `String`.** Profile names are compile-time constants — they're written once per consumer crate. `&'static str` keeps allocations off the resolver path.

**Why `CapabilityNotMet` carries `provider: &'static str`, not the backend reference.** The error is intended for operators reading logs. The provider name (e.g., `"RedisClusterCacheBackend"`) is enough to identify which plugin is bound; carrying a reference to the backend itself would prolong its lifetime and complicate the error type. `std::any::type_name_of_val` is a one-line helper that produces the type name.

**Why the resolver doesn't take a `&[&CacheCapability]` instead of `Vec<CacheCapability>`.** Builders accumulate state. The natural shape is `.require(...).require(...).resolve()`, where each `.require` adds to the internal `Vec`. A slice would force the consumer to construct an array literal once, which is uglier at the call site.

**Why we don't pre-validate at registration.** The SDK could check capabilities at `register_*_backend` time (when the wiring crate registers a backend). But the consumer's requirements aren't known then — only the consumer knows what it needs. Validation is naturally consumer-side, at resolver time.

**References:**

- ADR-001 — backend compatibility. The cache `consistency()`, leader-election `features().linearizable`, and lock `features().linearizable` checks reference the per-backend characteristics this ADR validates against.
- ADR-005 — facade + backend trait pattern. The fluent resolver returns the per-primitive facade; without per-primitive facades, the resolver couldn't express per-primitive return types.
- ADR-006 — builder/handle lifecycle. The resolver is invoked from inside the consumer's `RunnableCapability::start()`, which is what makes capability validation a startup check.
- DESIGN.md §3.6 (resolution pattern), §3.10 (capability validation).
- PRD.md §5.6 (consumer requirements and startup validation).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-clst-fr-validation-typed-profile` — Typed `ClusterProfile` marker; profile names live in one place per consumer crate.
- `cpt-cf-clst-fr-validation-capability-declarations` — Per-primitive `*Capability` enums replace bundled `CapabilityClass`.
- `cpt-cf-clst-fr-validation-honest-declaration` — Backend declares characteristics via `consistency()` / `features()`.
- `cpt-cf-clst-fr-validation-startup-fail` — `CapabilityNotMet { primitive, capability, provider }` fails resolution.
- `cpt-cf-clst-nfr-capability-validation` — Capability mismatch fails startup with actionable diagnostic.
- DESIGN §3.6 Resolution Pattern — Fluent resolver shape with `.profile(P).require(Cap).resolve()`.
- DESIGN §3.10 Capability Validation — Per-primitive characteristic checks; validation helpers per primitive.
- `cpt-cf-clst-component-sdk` (DESIGN §3.2) — SDK hosts the resolver builders and capability enums.
- `cpt-cf-clst-seq-per-primitive-resolution` (DESIGN §3.13) — Concrete resolution sequence diagram realizing this ADR.

**Sibling ADRs:**

- ADR-005 — Per-primitive facade is what the resolver returns.
- ADR-001 — Capabilities map to per-backend characteristics analyzed in ADR-001.
- ADR-009 — Capability validation is the defense-in-depth check that catches plugin lies about backend characteristics.
