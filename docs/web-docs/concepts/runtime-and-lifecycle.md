---
title: Runtime & lifecycle
description: Capabilities, the ordered lifecycle the runtime drives every gear through, async boundaries, and the cluster plane.
sidebar:
  label: Runtime & lifecycle
  order: 3
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

The runtime (`HostRuntime`) drives all gears through one shared, ordered sequence of phases:

```text
pre_init → DB migration → init → post_init → REST wiring → gRPC wiring → start → OoP spawn → wait for cancellation → stop
```

- **`pre_init`** — setup before migrations run (system gears only).
- **DB migration** — gear-owned migrations executed by the runtime (gears with `db` capability).
- **`init`** — build services, resolve dependencies via `ClientHub`, and register the gear's
  own SDK implementation.
- **`post_init`** — a **barrier**: it begins only after every gear's `init` has completed, so
  any cross-gear wiring is safe here (system gears only).
- **REST / gRPC wiring** — routes and gRPC services are registered.
- **`start`** — background work starts for stateful gears.
- **`OoP spawn`** — out-of-process gears are spawned after the gRPC hub is listening, so they
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

For multi-instance deployments the framework defines four coordination primitives behind
stable contracts — **distributed cache**, **leader election**, **distributed locks**, and
**service discovery** — where a consumer declares what it needs and the platform resolves it
against an operator-selected backend.

:::caution[Not yet implemented]
The cluster-plane primitives are designed but **not yet implemented** in the framework
source. Treat this section as roadmap; it will be documented for real once the
implementation lands.
:::
