---
status: accepted
date: 2026-08-13
---

# ADR-011: The Remote Backend Seam — One `ClusterClient` Per Process

**ID**: `cpt-cf-clst-adr-remote-backend-seam`

> Recorded with item `K4`, the change that routes every resolution through the seam. The pieces it names landed earlier — the trait in `C3`, the local implementation in `R5`, the remote one in `K2` — and the decision itself is [DESIGN.md](../DESIGN.md) §3.16, §3.17.7. This ADR is where it becomes a decision the codebase can be held to.

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Where the boundary goes](#where-the-boundary-goes)
  - [One object, and it is a factory](#one-object-and-it-is-a-factory)
  - [The `*Api` / `*Backend` split is load-bearing](#the-api--backend-split-is-load-bearing)
  - [Lazy binding, and what it must not hide](#lazy-binding-and-what-it-must-not-hide)
  - [Capability enforcement moves to the descriptor](#capability-enforcement-moves-to-the-descriptor)
  - [What this costs](#what-this-costs)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Cut at the facades](#option-1-cut-at-the-facades)
  - [Option 2: Cut at the primitive operations, one client per primitive](#option-2-cut-at-the-primitive-operations-one-client-per-primitive)
  - [Option 3: Cut at the three backend traits, one client per process (CHOSEN)](#option-3-cut-at-the-three-backend-traits-one-client-per-process-chosen)
- [More Information](#more-information)

<!-- /toc -->

## Context and Problem Statement

Cluster becomes deployable out of process. A consumer that today resolves a facade over a backend registered in its own `ClientHub` must, in a deployed topology, resolve the same facade over a gRPC channel to a cluster pod — with **no consumer source change**, because the point of the exercise is that one gear's code compiles and behaves identically in both.

So something has to differ, and the question is *what*, and *where*. Whatever object differs between the two deployments has to be the only thing that differs, has to be discoverable without any consumer-side configuration, and must not put a wrapper on the in-process hot path that Profile 1 pays for a capability it does not use.

## Decision Drivers

- **Profile transparency.** One consumer source file, no `cfg`, no mode flag (Goal 2, invariant I1).
- **The plugin contract is fixed.** The three `*Backend` traits are what every plugin implements; they cannot acquire a required method (invariant I11).
- **The in-process hot path must not regress.** Cluster is on the request path of gears that call it per operation (invariant I14).
- **Startup must not block on cluster reachability** (ADR-0005, invariant I6).
- **The error model is frozen.** No new variant for a condition the seam introduces (invariant I3).

## Considered Options

1. Cut at the facades — ship a `RemoteClusterCacheV1` beside `ClusterCacheV1`.
2. Cut at the primitive operations — one transport client per primitive, registered per profile.
3. **Cut at the three backend traits, with one `dyn ClusterClient` per process as their factory.**

## Decision Outcome

**Option 3.**

### Where the boundary goes

The process boundary is the three plugin-facing traits — `ClusterCacheBackend`, `DistributedLockBackend`, `LeaderElectionBackend`. A facade holds `Arc<dyn _Backend>` and cannot tell whether the object behind it reaches a local `BTreeMap` or a gRPC channel, because the trait is the same trait either way.

Everything above the seam is therefore untouched: the facades, the resolvers' shapes, `scoped()`, the watch unions, `auto_restart`, the guard types. Everything below it is a plugin concern that stays in the cluster gear's process, which is what keeps plugin linkage out of every consumer's binary.

### One object, and it is a factory

Exactly **one** `Arc<dyn ClusterClient>` is registered per process, and it is a *factory* for the three backend traits rather than a consumer-facing API of its own:

| Profile | Implementation | Registered by |
|---|---|---|
| 1 — embedded | `LocalClusterClient` over the gear's `ProfileRegistry` | the cluster gear's `start` (and `ClusterWiringBuilder::build_and_start` on the programmatic path) |
| 3 — deployed | `RemoteClusterClient` holding the channel and the descriptor cache | the SDK's `ConsumerRegistration` |

Local wins over remote, and it wins at **registration** time, not at resolve time: the consumer registration checks the hub before building a channel. There is no branch in the resolve path that could choose differently, which is what makes the split-brain hazard structurally absent rather than merely checked — a consumer cannot invent a local in-memory cache, because resolution can only ever resolve *toward* whatever was registered.

The factory methods are **synchronous and pure**. Locally a call is one `ArcSwap::load`, one `BTreeMap` lookup and an `Arc` clone that returns the *real* backend — no wrapper is interposed, so Profile 1 keeps its exact hot-path cost, and `local_client_tests` asserts that by pointer equality rather than describing it. Remotely a call constructs a handle: an `Arc` clone and an interned profile name, no I/O. `descriptor()` is the only `async` member, because remotely it needs one.

**The profile is a request parameter, not a wiring parameter.** Nothing profile-specific is registered; a remote implementation never learns which provider serves a profile, because the cluster gear resolves that server-side. That is what keeps a consumer from having to configure — or even know — what a profile means.

### The `*Api` / `*Backend` split is load-bearing

The wire contract traits (`ClusterCacheApi` and siblings) are **not** the backend traits, and collapsing them would be a mistake in both directions:

- The `*Api` traits carry a `PlatformSecurityContext` first parameter, `#[idempotency]`, DTO types and a `profile` argument on every method. None of that belongs on a trait a plugin implements.
- The `*Backend` traits carry `LockGuard`, `CacheWatch`, `LeaderWatch` — types with channels and drop semantics that have no wire projection at all, and which the remote handles *reconstruct* client-side from unary calls and streams.

Because the two are distinct, a contract change reaches the service impls and the remote handles and stops there; and because the *backend* traits are the seam, a plugin author never sees the wire. If a contract edit ever forces a change to a `*Backend` trait, the boundary has leaked and that is the finding.

### Lazy binding, and what it must not hide

Wiring order is not a dependency. `resolve()` tolerates an empty hub: it returns `Ok`, and the facade it hands back is built over an **unbound backend** whose every operation is `ProfileNotBound` naming the profile, and whose synchronous accessors answer with the weakest reading of every capability so nothing a consumer branches on is falsely satisfied.

Three rules keep that from trading a loud startup failure for a quiet runtime one:

1. A first call against an unbound facade is `ProfileNotBound { profile }` — the same variant a reachable server returns for a profile it does not bind, and **no new variant** (invariant I3). The distinguishing phrase, *no cluster client registered in this process*, is a `warn` log rather than part of the error: `ProfileNotBound`'s `Display` is frozen, so varying the message would mean widening exactly the variant that must not widen.
2. The requirement registry doubles as an unfeatured readiness contributor, so "nothing is wired" reaches `/readyz` instead of a request path. (Item `K5`; until it lands, the log and the first call are the whole report.)
3. **Resolve in `start`, never in `init`.** Framework phases are global: every gear's `init` runs before any gear's `start`, so the cluster gear's `start` — which registers the local client — has not run during any consumer's `init`.

### Capability enforcement moves to the descriptor

A remote handle cannot answer `consistency()` / `features()` / `provider_name()` without a `ProfileDescriptor`, so the descriptor becomes the single input to capability validation in **both** profiles rather than one path reading the backend and the other reading the wire. In-process the descriptor is computed from the real backends, so the verdict is unchanged and the error text is byte-identical across profiles — which is what the equivalence gate asserts.

`resolve()` awaits that descriptor on a **bounded** timeout (an SDK constant, 2 s) and validates inline when it lands. When it does not, validation defers to the readiness contributor with the same triple. Startup therefore never waits on cluster becoming reachable; it waits, briefly, on one descriptor. A *permanent* answer — the profile is not bound, a requirement is unmet — is returned from `resolve()` rather than deferred, because it cannot resolve on its own.

### What this costs

- **A wrapper on the remote path.** Every operation on a remote handle is a gRPC round trip; the arithmetic and its consequences are §6's, not this ADR's.
- **One descriptor fetch per profile at startup**, and the cold-start hole it leaves: a consumer that calls `consistency()` after a timed-out resolve reads the fail-safe answer rather than the true one. Contained by readiness gating — no consumer respecting `/readyz` observes it — and accepted as a programming error by a consumer that started working before it was ready.
- **`register_*_backend` alone no longer makes a profile resolvable.** The scoped hub entries remain the process-local binding record and the identity check that the local client interposes nothing, but the thing a facade resolves through is the client. Anything that wired by hand goes through `ClusterWiring` instead — which is the code that ships, and so the better fixture regardless.

### Consequences

- One trait object per process, so `ClientHub`'s ordinary one-dependency shape applies to cluster unchanged; cluster's only variation is that its object is a factory for three traits rather than the API itself.
- Profile 1 pays nothing: no wrapper, no atomic load, no hub lookup per operation.
- The transport is linked only where it is used — the remote implementation is behind `grpc-client`, and a Profile 1 monolith does not enable it.
- A consumer's source file is identical in both profiles, which is the property this whole seam exists to buy.

### Confirmation

- `local_client_tests` asserts by `Arc::ptr_eq` that the local factory hands back the registry's own instance for all three primitives — a wrapper would be invisible in review and would tax every embedded cache operation.
- `binding_tests` asserts the four steps: the backend comes back un-interposed, an empty hub resolves `Ok` and reports on first call, a client that does not bind the profile fails loudly, a descriptor that never arrives is bounded by the timeout (asserted on a paused clock, so a real wait would hang the test rather than pass it), and a permanent descriptor error is returned rather than deferred.
- `cluster/tests/remote_backends.rs` runs every assertion through `Arc<dyn _Backend>` against a real socket, several of them comparing the remote answer to the local backend's.
- The conformance suite over the wire (item `T1`) is the standing check that the two sides of the seam cannot drift.

## Pros and Cons of the Options

### Option 1: Cut at the facades

Ship `RemoteClusterCacheV1` beside `ClusterCacheV1`.

- Bad, and decisively: every consumer names the facade type, so a deployment change becomes a source change in every consumer. It fails Goal 2 outright.
- Bad: two implementations of every facade method — scoping, watch seams, guard lifetimes — to keep in step by hand.
- Good: the transport could be shaped per method without a trait to satisfy. Not worth it.

### Option 2: Cut at the primitive operations, one client per primitive

Register a transport client per `(profile, primitive)`.

- Good: superficially the platform's usual shape.
- Bad: it puts profile knowledge back on the consumer side — something has to know which client serves which profile, which is exactly the mapping only the cluster gear can make.
- Bad: three registrations per profile, and a consumer resolving a profile that gained a primitive would need re-wiring rather than a server-side config change.
- Bad: it multiplies channels; one per primitive per profile, where one per process suffices.

### Option 3: Cut at the three backend traits, one client per process (CHOSEN)

- Good: the facades, the resolvers' shapes and the plugin contract are all untouched.
- Good: one object to register, one channel to build, zero consumer configuration.
- Good: Profile 1 interposes nothing at all — the factory returns the real `Arc`.
- Good: the profile stays a request parameter, so profile knowledge stays where the plugins are.
- Bad: a factory is a slightly unusual thing to find behind a `dyn Trait` in this codebase, which is why it is written down here.
- Bad: the descriptor is a genuine up-front interaction on the remote path — the one thing that is not lazy — because three synchronous accessors on a frozen trait have to be answerable.

## More Information

- [DESIGN.md](../DESIGN.md) §3.16 (where the boundary goes), §3.10.1 (the bounded await and the inline-vs-deferred split), §3.17.7 (consumer-side wiring), §3.18.4 (profile discovery).
- [ADR-005](./005-facade-plus-backend-trait-pattern.md) — the facade/backend split this seam cuts along.
- [ADR-007](./007-capability-typing-and-profile-resolution.md) — the typed profile marker, which is what a resolver still names.
- [ADR-012](./012-store-owned-leases.md) — why a lease survives the replica it was acquired through, which is what makes a remote handle's token meaningful.
