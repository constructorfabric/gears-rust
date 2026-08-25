---
status: accepted
date: 2026-04-27
---

# ADR-006: Outbox-style Builder/Handle Lifecycle Owned by Parent Host Gear

**ID**: `cpt-cf-clst-adr-builder-handle-lifecycle`

<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [What "the wiring crate" is and is not](#what-the-wiring-crate-is-and-is-not)
  - [Plugin handles are nested under the cluster handle](#plugin-handles-are-nested-under-the-cluster-handle)
  - [Shutdown sequence](#shutdown-sequence)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Option 1: Two-tier `RunnableCapability` — wiring as one capability, each plugin as a separate one](#option-1-two-tier-runnablecapability--wiring-as-one-capability-each-plugin-as-a-separate-one)
  - [Option 2: Single bundled `RunnableCapability` — plugins as constants inside the wiring impl](#option-2-single-bundled-runnablecapability--plugins-as-constants-inside-the-wiring-impl)
  - [Option 3: Outbox-style builder/handle owned by parent host gear (CHOSEN)](#option-3-outbox-style-builderhandle-owned-by-parent-host-gear-chosen)
  - [Option 4: Ad-hoc lifecycle — each plugin spawns/joins its own tasks, no central coordinator](#option-4-ad-hoc-lifecycle--each-plugin-spawnsjoins-its-own-tasks-no-central-coordinator)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

## Context and Problem Statement

The cluster wiring crate (`cf-cluster`, follow-up change) is the layer that reads operator YAML, instantiates per-backend plugins, and registers each plugin's `Arc<dyn _Backend>` in ClientHub under the corresponding profile scope. Plugins (Postgres, K8s, Redis, NATS, etcd, standalone) own background tasks: TTL reapers for SDK-default lock backends, renewal loops for leader-election backends, watch fan-out for cache backends.

Two intertwined questions arise:

1. **Who owns the wiring crate's lifecycle?** ToolKit's standard pattern is `RunnableCapability` — a trait whose `start(cancel) -> Result<()>` and `stop(cancel) -> Result<()>` are invoked by the framework. If the wiring crate is its own `RunnableCapability`, the framework decides when wiring starts and when it stops, and we get framework-managed start/stop ordering between wiring and consumer gears.

2. **Who owns plugin lifecycles inside the wiring crate?** A naive implementation makes each plugin its own `RunnableCapability` too, with the framework ordering plugins after wiring. Less naive: the wiring crate spawns each plugin's background tasks during its own `start()` and joins them during `stop()`. Or: the wiring crate is a *library*, not a `RunnableCapability` at all, and a parent host gear owns it via plain code-flow ordering inside its own `start()`/`stop()`.

The first instinct is the framework-native one — make wiring and each plugin their own `RunnableCapability` and let ToolKit order them. This sounds clean but introduces a coordination problem the framework was not designed to solve: cross-capability lifecycle ordering between wiring (which must register backends in ClientHub before consumers can resolve) and plugins (whose backends register from their own `start()`). ToolKit's dependency mechanism orders gear starts; it does not order capability starts within a gear's lifecycle. Building that ordering inside ToolKit is significant infra work for one consumer.

The mature alternative is already in the codebase: **the outbox pattern**. `cluster/libs/toolkit-db/src/outbox/manager.rs` is a long-running background-task component owned by its consumer gear (mini-chat) via `Outbox::builder(...).start()` from inside the consumer's `RunnableCapability::start()`. The consumer holds the resulting `OutboxHandle` and calls `handle.stop()` from its own `stop()`. No framework-level capability ordering required — code flow inside the consumer's `start()` is the ordering.

Cluster wiring fits this shape exactly. This ADR captures why the outbox pattern is the right choice and why the framework-native `RunnableCapability` per-plugin shape is wrong.

## Decision Drivers

- **Avoid framework changes**: ToolKit currently has no cross-`RunnableCapability` lifecycle ordering primitive within a gear. Building one for cluster's benefit is heavy and would set a precedent that doesn't match the rest of the platform.
- **Reuse proven prior art**: the outbox pattern has been in production for the mini-chat gear's transactional outbox. It is the codebase's mature long-running-background-task pattern.
- **Code-flow ordering is sufficient**: inside one parent gear's `start()`, line-by-line execution is a perfectly good ordering primitive. Wiring starts before plugins start, plugins start before backends register, backends register before any consumer can resolve. Sequential await calls express this directly.
- **Single shutdown entry point**: every cluster artifact must be released by one `stop()` call. Multiple `RunnableCapability` impls fragment the shutdown story into N stop calls in framework-determined order.
- **Plugin authors should not write framework integration code**: plugin authors implement backend traits and a builder/handle pair. They should not have to know about `RunnableCapability`, dependency declarations, or framework hooks.

## Considered Options

1. **Two-tier `RunnableCapability`** — wiring as one capability, each plugin as a separate one, framework orders them.
2. **Single bundled `RunnableCapability`** — wiring is a `RunnableCapability`; plugins are constants/structs inside its impl, started/stopped from inside the wiring's start/stop.
3. **Outbox-style builder/handle owned by parent host gear** — wiring is a library, not a `RunnableCapability`. A parent host gear's `start()` calls `ClusterWiring::builder(...).build_and_start()` and stores the resulting `ClusterHandle`; its `stop()` calls `handle.stop()`. (CHOSEN.)
4. **Ad-hoc lifecycle** — no central coordinator; each plugin spawns/joins its own tasks; cluster has no single shutdown entry point.

## Decision Outcome

Chosen option: **Option 3** — outbox-style builder/handle, parent-host-gear-owned.

The cluster wiring crate (`cf-cluster`) is **not** a `RunnableCapability`. It is a library exposing:

```rust
impl ClusterWiring {
    pub fn builder(config: &ClusterConfig, hub: &ClientHub) -> ClusterWiringBuilder;
}

impl ClusterWiringBuilder {
    pub async fn build_and_start(self) -> Result<ClusterHandle, ClusterError>;
}

impl ClusterHandle {
    pub async fn stop(self) -> ();
}
```

A parent host gear — registered as a `RunnableCapability` in the usual ToolKit way — owns the `ClusterHandle`:

```rust
impl RunnableCapability for HostGear {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        let cluster_handle = ClusterWiring::builder(&self.config.cluster, &self.hub)
            .build_and_start()
            .await?;
        self.cluster_handle.set(cluster_handle).ok();
        Ok(())
    }

    async fn stop(&self, cancel: CancellationToken) -> anyhow::Result<()> {
        if let Some(handle) = self.cluster_handle.take() {
            tokio::select! {
                () = handle.stop() => {}
                () = cancel.cancelled() => {}
            }
        }
        Ok(())
    }
}
```

Consumer gears (event broker, OAGW, scheduler, etc.) are registered as ToolKit dependents of the parent host gear. ToolKit's existing gear-dependency mechanism guarantees the host's `start()` completes before any consumer's `start()` runs — by the time a consumer attempts to resolve `ClusterCacheV1::resolver(hub).profile(P).resolve()`, the wiring's backends are already registered in ClientHub.

### What "the wiring crate" is and is not

The wiring crate is:

- A library that reads `ClusterConfig` (operator YAML deserialized), iterates the `profile × primitive` matrix, instantiates the right plugin for each cell, and registers each `Arc<dyn _Backend>` in ClientHub under `profile_scope(profile_name)`.
- The omit-primitive auto-wrap layer: when a profile binds `cache` but omits `leader_election`, the wiring crate auto-wraps the cache backend in `CasBasedLeaderElectionBackend` (per ADR-001) and registers it.
- The owner of the `ClusterHandle` returned to the parent gear.

The wiring crate is NOT:

- A `RunnableCapability`. It does not implement that trait.
- A holder of consumer state. It exposes ClientHub as the single integration point; consumers resolve from ClientHub, not from a wiring-crate accessor.
- A runtime compositor object. It does not own `Arc<dyn Cluster>` or any bundled cluster handle that consumers reach through. After `build_and_start()` returns, the wiring crate's only role is to keep plugin handles alive until `stop()`.

### Plugin handles are nested under the cluster handle

Each plugin (Postgres, K8s, Redis, NATS, etcd, standalone) exposes a builder/handle pair of its own:

```rust
impl PostgresClusterPlugin {
    pub fn builder(...) -> PostgresClusterPluginBuilder;
}

impl PostgresClusterPluginBuilder {
    pub async fn build_and_start(self) -> Result<PostgresClusterPluginHandle, ...>;
}

impl PostgresClusterPluginHandle {
    pub async fn stop(self) -> ();
}
```

The cluster wiring's `build_and_start()` calls each needed plugin's `build_and_start()` in turn, registers the plugin's backend(s) in ClientHub, and stores the plugin's handle inside the `ClusterHandle`'s internal vector. `ClusterHandle::stop()` calls each plugin handle's `stop()` in reverse-start order.

Plugins are NOT separate `RunnableCapability` implementors. Plugin authors implement two things: a backend trait per primitive they serve, and a builder/handle pair following this pattern. Plugin authors do not write framework integration code.

### Shutdown sequence

`ClusterHandle::stop().await` is the single shutdown entry point. It performs:

1. **Deregister all backends from ClientHub.** After this step, any subsequent `*V1::resolver(...).resolve()` on the parent profile fails with `ClusterError::ProfileNotBound`. Consumers in flight may still hold `Arc`-cloned facades from earlier resolutions.
2. **Stop nested plugin handles in reverse-start order.** Each plugin handle's `stop()` cancels its `CancellationToken`, joins its background tasks (TTL reapers, renewal loops, watch fan-out), and returns. Plugin handles are independent — a stuck plugin does not block the others (they run with bounded `tokio::select!` against the parent host's `cancel` signal at the outer layer).
3. **Deliver terminal watch events** to active watches in the order specified by ADR-003: `LeaderWatchEvent::Status(Lost)` then `LeaderWatchEvent::Closed(Shutdown)` for active leaders; and `CacheWatchEvent::Closed(Shutdown)` for cache watches.

Step 1 happens before steps 2-3 to ensure no new resolutions race against a partially shut down plugin set. Steps 2 and 3 are interleaved per plugin: the plugin's `stop()` is what delivers terminal watch events, then joins the background task that owned the watch.

Post-shutdown best-effort `Ok` semantics: `LockGuard::release(self)` / `LeaderWatch::resign(self)` MAY return `Ok(())` after their plugin handle has observed shutdown — the resource is conceptually released, the bookkeeping is moot. Outside the shutdown window, real errors (foreign-holder release attempts, connection-lost mid-release, `LockExpired`) propagate normally. This narrowed best-effort `Ok` prevents shutdown noise from masquerading as consumer bugs.

### Consequences

- **Single shutdown entry point**: parent gear's `RunnableCapability::stop` calls `handle.stop()`. One line. Everything cluster owns is released through that one call.
- **No framework changes**: ToolKit doesn't need a cross-capability lifecycle ordering primitive. The parent gear's existing `RunnableCapability` is the ordering primitive — line-by-line execution inside its `start()` and `stop()`.
- **Plugin authors write less**: a plugin is one trait impl plus a builder/handle pair. No `RunnableCapability` impl, no `cancel: CancellationToken` parameter to plumb through framework hooks, no dependency declarations.
- **Code-flow ordering is explicit and reviewable**: the parent gear's `start()` shows wiring start, plugin start, backend registration, and consumer-readiness as sequential await points. A reviewer can read the file top-to-bottom and see the ordering.
- **Reverse-start shutdown**: plugin handles stop in reverse-start order naturally because the wiring crate stores them in a `Vec` and pops from the end. No declarative ordering needed.
- **Nested handle structure mirrors nested ownership**: `ClusterHandle` owns plugin handles; each plugin handle owns its own background tasks. The Rust ownership tree matches the lifecycle tree.
- **Consumer-readiness is a ToolKit dependency, not a cluster concern**: the parent host gear declares itself a ToolKit dependency of consumer gears. Consumers can't `start()` until the host gear finishes `start()`, by which time backends are registered.
- **Trade-off**: this design assumes a single owner of the `ClusterHandle`. Two consumers cannot each "own" the cluster — only the parent host gear does. This matches reality (cluster is a singleton platform-tier infrastructure), but the constraint is worth being explicit about.

### Confirmation

- A unit test instantiates the wiring crate against an in-memory plugin (standalone), calls `build_and_start()`, resolves all three primitives, calls `handle.stop()`, and verifies subsequent resolutions return `ProfileNotBound`.
- An integration test exercises the shutdown sequence: spawn an active `LeaderWatch`, call `handle.stop()`, assert the watch observes `Status(Lost)` followed by `Closed(Shutdown)` in that order.
- A drop-test verifies that if the parent gear is dropped without calling `handle.stop()` (a programming error), background tasks are NOT silently leaked — `Drop` on `ClusterHandle` is panic-safe (`std::thread::panicking()` guard) and emits a debug-build panic / release-build warn-log to surface the bug:
    ```rust
    impl Drop for ClusterHandle {
        fn drop(&mut self) {
            if std::thread::panicking() {
                tracing::warn!(
                    "ClusterHandle dropped during panic unwind without stop(); \
                     skipping debug panic to avoid double-panic abort"
                );
                return;
            }
            #[cfg(debug_assertions)]
            panic!("ClusterHandle dropped without stop() — programming error");
            #[cfg(not(debug_assertions))]
            tracing::warn!(
                "ClusterHandle dropped without stop() — programming error; \
                 background tasks may leak"
            );
        }
    }
    ```
- A panic-while-panicking test asserts the `thread::panicking()` guard fires: code panics inside a closure that owns a `ClusterHandle`, and the test verifies the process does NOT abort (would happen on double-panic) and the warn-log is emitted instead of the debug-build panic.
- A timeout test verifies that a stuck plugin (one that hangs in its `stop()`) does not prevent the parent gear's `cancel` deadline from firing — the `tokio::select!` in the parent's `stop()` cuts off after the framework-supplied deadline.

## Pros and Cons of the Options

### Option 1: Two-tier `RunnableCapability` — wiring as one capability, each plugin as a separate one

```rust
// Wiring crate
impl RunnableCapability for ClusterWiring { ... }

// Each plugin crate
impl RunnableCapability for PostgresClusterPlugin { ... }
impl RunnableCapability for K8sClusterPlugin { ... }
```

Framework declares: `ClusterWiring` runs after all plugins; consumer gears run after `ClusterWiring`.

- Good, because framework-native — uses ToolKit's existing `RunnableCapability` everywhere.
- Bad, because ToolKit has no cross-capability ordering primitive within a gear's lifecycle. The wiring's `start()` needs to know that all plugins' `start()` calls have completed; without a framework-supplied "wait for these capabilities to start" hook, this requires building that infrastructure.
- Bad, because building that ordering infra inside ToolKit for one consumer's benefit is a heavy lift — and would set a precedent for cluster-shaped capabilities that don't actually exist anywhere else in the platform.
- Bad, because shutdown fragments into N independent `stop()` calls in framework-determined order. The shutdown sequence (terminal watch events, deregister-then-stop ordering — see ADR-003) is not expressible as N independent capability stops; it requires explicit coordination.
- Bad, because plugin authors must write `RunnableCapability` impls — significantly more framework integration code per plugin.
- Bad, because every plugin's lifecycle is now a framework-visible artifact. Adding a plugin to `Cargo.toml` is no longer enough; you also have to wire its `RunnableCapability` registration into the host gear.

### Option 2: Single bundled `RunnableCapability` — plugins as constants inside the wiring impl

```rust
impl RunnableCapability for ClusterWiring {
    async fn start(&self, _cancel: CancellationToken) -> anyhow::Result<()> {
        // start plugins inline
        let pg = PostgresClusterPlugin::start(&self.config.postgres).await?;
        let k8s = K8sClusterPlugin::start(&self.config.k8s).await?;
        // register backends
        self.hub.register(...);
        Ok(())
    }
    async fn stop(&self, _cancel: CancellationToken) -> anyhow::Result<()> { ... }
}
```

- Good, because single shutdown entry point and clear ordering inside the wiring's start/stop.
- Good, because plugin authors don't write framework code — they ship builders.
- Bad, because the wiring crate is now a `RunnableCapability`, which means the framework decides when wiring starts. ToolKit's gear-dependency mechanism orders gears; the wiring crate would have to be a gear of its own (with its own crate, dependencies, etc.) just to participate.
- Bad, because consumer gears' "start after cluster wiring" requirement becomes "depend on the cluster-wiring gear" — which works, but is more declarative ceremony than just having the parent host gear own the wiring directly.
- Neutral, because this option is structurally similar to Option 3; the only real difference is whether the framework drives `start()`/`stop()` (Option 2) or a parent gear does (Option 3). Option 3 wins on framework simplicity (no new gear needed for wiring) and on prior-art consistency (outbox already does it this way).

### Option 3: Outbox-style builder/handle owned by parent host gear (CHOSEN)

```rust
// Wiring crate exposes a library API — no RunnableCapability impl
let cluster_handle = ClusterWiring::builder(&config, &hub).build_and_start().await?;
// Parent gear's stop()
cluster_handle.stop().await;
```

- Good, because matches the codebase's mature long-running-background-task pattern (outbox in `cluster/libs/toolkit-db`, owned by mini-chat). Not a new pattern — a proven one.
- Good, because no framework changes needed. The parent host gear is a regular `RunnableCapability`; the cluster lives inside it.
- Good, because plugin authors write the same builder/handle shape they would for any background-task crate. Framework-agnostic.
- Good, because shutdown is one method call (`handle.stop()`). The shutdown sequence (terminal watch events, deregister-before-stop) is implemented inside that one method, not coordinated across N capability stops.
- Good, because code-flow ordering inside the parent gear's `start()` is explicit and reviewable. Sequential awaits express the order: build wiring, register backends, signal readiness. A reviewer reads the file top-to-bottom.
- Good, because the design composes cleanly with ToolKit's gear-dependency mechanism — consumer gears declare the parent host as a dependency, ToolKit guarantees ordering, no new framework primitive needed.
- Bad, because consumers cannot resolve cluster artifacts from inside their own `start()` *before* the parent host's `start()` completes. Mitigated by ToolKit's existing gear-dependency ordering — this is exactly the problem ToolKit's dependency mechanism solves, and we use it.
- Bad, because if a parent gear forgets to call `handle.stop()` from its own `stop()`, plugin background tasks leak. Mitigated by `Drop` on `ClusterHandle` (debug-build panic guarded by `std::thread::panicking()` to avoid double-panic abort during stack unwinding; release-build warn-log) and by the obvious symmetry of `build_and_start` ↔ `stop`.
- Neutral, because the parent host gear is one extra gear that has to exist. In practice, the parent host gear is the same gear that already owns ClientHub registration setup — it's not new infrastructure.

### Option 4: Ad-hoc lifecycle — each plugin spawns/joins its own tasks, no central coordinator

- Good, because zero shared infrastructure.
- Bad, because there is no single shutdown entry point. The parent gear would have to know about every plugin individually and stop them one by one.
- Bad, because shutdown ordering between plugins becomes the parent gear's problem — a problem the wiring crate is supposed to abstract.
- Bad, because terminal watch event delivery (per ADR-003) requires coordination across plugins; no single owner means no single place to coordinate.
- Bad, because adding a new plugin requires the parent gear to know about it. The wiring layer's whole point is that adding a plugin is a config change, not a code change.

## More Information

**Why "outbox-style" specifically.** The outbox pattern in `cluster/libs/toolkit-db/src/outbox/manager.rs` (see `OutboxBuilder::start` and `OutboxHandle::stop`) is the codebase's reference implementation of the builder/handle pattern for long-running async work. It exposes `Outbox::builder(...)` returning a builder with `.start()` (note: outbox uses `.start()`; cluster wiring uses `.build_and_start()` to make the build-then-start composition obvious), and the resulting `OutboxHandle` exposes `.stop().await`. Mini-chat owns the handle from inside its own `RunnableCapability::start`/`stop`. That same shape — builder produces handle, handle's `stop()` is the single release path, parent gear owns the handle — is what cluster wiring adopts.

**Why not just a `Drop` impl on `ClusterHandle` that does cleanup.** Cleanup is async (deregister backends, stop plugin tasks, deliver terminal watch events). `Drop` is sync. Per ADR-002, no I/O in `Drop`. The handle has `stop()` as an explicit async method; `Drop` is reserved for diagnostic warning if the handle is dropped without stopping.

**Why the parent host gear is "out of scope of this change".** The cluster gear ships the SDK and wiring crate. The parent host gear is a thin shim — its only job is to own `ClusterHandle`. Whether it lives in the gateway crate, a dedicated `cf-cluster-host` crate, or each cluster-using product's own host gear is a deployment-shape decision orthogonal to the cluster contract. Different deployments may pick differently. The cluster contract just says "someone owns the handle from `RunnableCapability::start`/`stop`."

**References:**

- ADR-001 — backend compatibility and the cache-CAS-universal model. The omit-primitive auto-wrap behavior is implemented inside `ClusterWiring::build_and_start()`.
- ADR-002 — async boundary, no I/O in `Drop`. Why `ClusterHandle::stop()` is an explicit async method, not a `Drop` impl.
- ADR-003 — watch event lifecycle contract. The shutdown sequence (terminal watch events) lives inside `ClusterHandle::stop()`.
- ADR-005 — facade + backend trait pattern. Plugins implement backend traits; the wiring crate registers each `Arc<dyn _Backend>` in ClientHub.
- DESIGN.md §3.7 (lifecycle pattern), §3.11 (SDK default backends and omit-primitive auto-wrap as wiring-crate behavior), §3.13 (shutdown sequence diagram).
- Prior art: `cluster/libs/toolkit-db/src/outbox/manager.rs` (the outbox pattern's reference implementation).

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements and design elements:

- `cpt-cf-clst-fr-lifecycle-owner` — Single owner of the cluster lifecycle (parent host gear).
- `cpt-cf-clst-fr-shutdown-revoke` — `ClusterHandle::stop()` as the single shutdown entry point.
- `cpt-cf-clst-fr-watch-lifecycle-signals` — Terminal watch event delivery during shutdown.
- `cpt-cf-clst-fr-routing-per-primitive` — Wiring crate's per-primitive backend registration enables routing.
- `cpt-cf-clst-component-wiring` (DESIGN §3.2) — `cf-cluster` library exposes `ClusterWiring::builder(...).build_and_start()`.
- `cpt-cf-clst-component-plugins` (DESIGN §3.2) — Plugins are nested builder/handle pairs owned by `ClusterHandle`.
- `cpt-cf-clst-seq-lifecycle-startup` (DESIGN §3.13) — Concrete startup sequence diagram realizing this ADR.
- `cpt-cf-clst-seq-shutdown` (DESIGN §3.13) — Concrete shutdown sequence diagram realizing this ADR.
- DESIGN §3.7 Lifecycle Pattern (Builder/Handle) — Architecture-level realization.
- DESIGN §3.11 SDK Default Backends — Omit-primitive auto-wrap is implemented inside `ClusterWiring::build_and_start()`.

**Sibling ADRs:**

- ADR-002 — `Drop` panic guard inherits the no-I/O-in-Drop rule.
- ADR-003 — Shutdown sequence delivery order (Status(Lost) → Closed(Shutdown)) is implemented inside `ClusterHandle::stop()`.
- ADR-005 — Plugins as builder/handle pairs is the corollary of the per-primitive backend trait surface.
