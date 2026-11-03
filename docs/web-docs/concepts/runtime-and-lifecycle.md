---
title: Runtime & lifecycle
description: Capabilities, the ordered lifecycle the runtime drives every gear through, async boundaries, and the cluster plane.
sidebar:
  label: Runtime and lifecycle
  order: 4
---

## Capabilities

A gear declares what it needs and what it exposes as **capabilities** in its
`#[toolkit::gear(...)]` attribute:

- `db` — needs a database; implements `DatabaseCapability` (and provides migrations).
- `rest` — exposes a REST API; implements `RestApiCapability`.
- `grpc` — exposes a gRPC service (used by out-of-process gears).
- `stateful` — runs background work with a managed lifecycle.

The runtime discovers every gear at link time (via `inventory`), builds a dependency-ordered
registry from the declared `deps`, and wires the system from those declarations.

## The lifecycle

The runtime (`HostRuntime`) drives gears in two groups. The **system group** — the few
platform gears with the `system` capability (types registry, service discovery, cluster,
API gateway, gRPC hub) — runs its whole startup first; the **application group** (every
other gear) follows. Each group runs the same ordered phases, every gear's hooks once, in
dependency order:

```text
system group:      pre_init → DB migration → init → client wiring → post_init → REST/gRPC → start
                   → OoP spawn
application group: pre_init → DB migration → GTS publication (admission barrier) → init
                   → client wiring → post_init → REST/gRPC → start
                   → wait for cancellation → stop (application group, then system group)
```

- **`pre_init`** — optional hook of any gear before its group's migrations run.
- **DB migration** — gear-owned migrations executed by the runtime (gears with `db` capability).
- **GTS publication** — the GTS types and instances application gears declare with `gts_publication(...)`
  are published concurrently and admitted before any application `init`.
- **`init`** — build the gear's own services and register its own SDK implementation in the
  `ClientHub`. Ordinary consumed clients are wired only after every `init` of the group, so
  keep handles for lazy use (or resolve them in `post_init`) rather than calling another gear.
- **`post_init`** — optional hook of any gear; a **barrier** within its group: it begins only
  after every `init` of the group completed and its consumed clients are wired.
- **REST / gRPC wiring** — routes and gRPC services are registered.
- **`start`** — background work starts for stateful gears.
- **`OoP spawn`** — out-of-process gears are spawned once the system group started, so they
  can connect to the directory endpoint.
- **Wait for cancellation** — the runtime blocks until a cancellation signal is received
  (Ctrl-C, SIGTERM, or programmatic).
- **`stop`** — shutdown runs in **reverse dependency order** with a platform deadline.
  Cancellation tokens propagate so background tasks cooperate with shutdown rather than
  outliving the host.

This gives every gear one predictable operational model — stable ordering, shared
cancellation semantics, and consistent startup/shutdown — instead of ad-hoc lifecycle code.

![Runtime lifecycle](../assets/runtime-lifecycle.drawio.svg)

## Async boundaries

Gears are async Rust. The framework keeps async correctness enforceable at compile time:
strict Clippy rules (e.g. `await_holding_lock`, `await_holding_refcell_ref`,
`async_yields_async`) are denied across the workspace, so an entire class of async bugs is
caught in CI rather than in production. Background tasks are cancellation-aware and bounded
by the shutdown deadline (see the lifecycle above).

## The cluster plane (planned)

For multi-instance deployments the framework defines three coordination primitives behind
stable contracts — **distributed cache**, **leader election**, and **distributed locks** —
where a consumer declares what it needs and the platform resolves it against an
operator-selected backend.

:::caution[Not yet implemented]
The cluster-plane primitives are designed but **not yet implemented** in the framework
source. Treat this section as roadmap; it will be documented for real once the
implementation lands.
:::
