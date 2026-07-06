---
title: System overview
description: How the Gears runtime, request path, and deployment shapes are organized.
sidebar:
  label: System overview
  order: 1
---

This is how a Gears system is organized end to end. It expands on the
[core concepts](../concepts/) with the runtime lifecycle, the request path, and the
deployment model.

## Three tiers, one-way dependencies

Gears compose in three tiers — **Toolkit** (`libs/`), **System gears**
(`gears/system/`), and **Service gears** (`gears/`) — with dependencies flowing in one
direction only (service → system → toolkit). Because gears are discovered at link time and
ordered by their declared dependencies, adding a gear never requires editing a central
registration point; dependency ordering is a platform guarantee.

## Runtime lifecycle

The runtime (`HostRuntime`) discovers every gear, builds a dependency-ordered registry, and
drives all gears through one shared sequence of phases:

```text
pre_init → DB migration → init → post_init → REST wiring → gRPC wiring → start → OoP spawn → wait for cancellation → stop
```

1. **`pre_init`** — setup before migrations run (system gears only).
2. **DB migration** — gear-owned migrations executed by the runtime (gears with `db` capability).
3. **`init`** — build services, resolve dependencies via `ClientHub`, register the gear's
   own SDK implementation.
4. **`post_init`** — a **barrier**: it begins only after every gear's `init` has completed,
   so cross-gear wiring is safe here (system gears only).
5. **REST wiring** — routes registered with the API Gateway (gears with `rest` capability).
6. **gRPC wiring** — gRPC services registered with the gRPC hub (gears with `grpc` capability).
7. **`start`** — background work starts for stateful gears.
8. **OoP spawn** — out-of-process gears are spawned after the gRPC hub is listening.
9. **Wait for cancellation** — the runtime blocks until a cancellation signal (Ctrl-C, SIGTERM).
10. **`stop`** — shutdown runs in **reverse dependency order** with a platform deadline.
    Cancellation tokens propagate so background tasks cooperate with shutdown instead of
    outliving the host.

![Runtime lifecycle](../assets/runtime-lifecycle.drawio.svg)

A gear opts into work in each phase by declaring **capabilities** (`db`, `rest`, `grpc`,
`stateful`) and implementing the matching capability traits (e.g. `DatabaseCapability`,
`RestApiCapability`).

## Request lifecycle

A request flows through clearly separated responsibilities:

```text
Client
  → API Gateway        validates the token → SecurityContext; checks license
  → Gear handler       calls PolicyEnforcer (PEP)
      → AuthZ Resolver (PDP) returns decision + row-level constraints → AccessScope
  → SecureConn         applies AccessScope as automatic WHERE clauses
  → domain service     business logic
  → response           (RFC-9457 problem on error)
```

The API Gateway owns authentication and license validation; gear domain services own
authorization. Gear code never parses tokens or resolves tenancy directly — it receives a
`SecurityContext` and asks the `PolicyEnforcer` for an `AccessScope`.

## The SDK + backend pattern

Each gear's public surface is its SDK crate (a facade trait + models + errors). Behind that
trait, the runtime can wire different **backends**:

- an **in-process adapter** registered in `ClientHub`, or
- a generated **gRPC client** that talks to the gear in another process.

Consumers resolve `hub.get::<dyn SomeClientV1>()` and call the trait; which backend they get
is decided by configuration. This is what lets the same code run in any deployment shape.

## Gateway & contract

The **API Gateway** terminates TLS (optionally), validates JWTs into a `SecurityContext`,
and routes to the target gear. Contracts are **code-first**: route metadata declared with
`OperationBuilder` (method, path, auth, schemas, errors) is collected by an
`OpenApiRegistry` and the OpenAPI document is generated from it — so the spec is in sync
with the code by construction, and REST clients can be generated from the same contract.

## Secure-by-default data path

Security is a layered path with no unscoped shortcut:

1. **Static checks** — custom architecture lints (via `cargo gears lint`) enforce architecture rules (DTO placement, domain
   isolation, no raw SQL outside migrations, versioned paths, mandatory `OperationBuilder`
   metadata) at build time.
2. **Authentication** — tokens validated at the gateway; `SecurityContext` injected.
3. **Authorization** — `PolicyEnforcer` → PDP → constraints compiled to `AccessScope`.
4. **Database scoping** — `SecureConn` applies the scope as `WHERE` clauses.
5. **Credentials & egress** — secrets via the credstore gear; outbound HTTP via the OAGW.

## Multi-tenancy

Tenants form a **single-root tree**. Every resource carries an `owner_tenant_id` — the
primary isolation boundary. A materialized **closure table** makes ancestor/descendant
queries cheap. Parents see child data by default; a child raises a **barrier**
(`self_managed = true`) to hide its subtree, configurable per resource type. **Resource
groups** add optional, tenant-scoped grouping used as an input to authorization decisions.

## Deployment shapes

One codebase, three shapes — chosen by configuration, not by changing code:

| Shape | Where gears run | How they talk |
| --- | --- | --- |
| **Single-node** | one process (edge, on-prem, dev) | in-process via `ClientHub` |
| **Multi-node** | across processes/machines, no orchestrator | gRPC, `SecurityContext` over headers |
| **Kubernetes** | containerized services | cluster DNS discovery, external gateways |

What changes between them is configuration (`runtime.type`, backend selection, bootstrap
entry point) — the gear code is identical.

## Design principles

- **Secure by default** — every handler enforces authn, authz, tenant isolation, and scoped
  DB access; the platform owns the security data path.
- **Explicit over implicit** — declared capabilities and dependencies, no hidden globals.
- **SDK-first contracts** — public API in an SDK crate; implementations depend on it, never
  the reverse.
- **One runtime, many shapes** — gear logic lives in libraries; binaries compose them for a
  deployment shape.
- **Governed evolvability** — canonical errors, versioned APIs, and the Global Type System
  let the domain grow without breaking existing gears.
