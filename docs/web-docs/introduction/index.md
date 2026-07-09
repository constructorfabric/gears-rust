---
title: What is Gears?
description: Gears is a Rust framework for building composable, secure-by-default platform components.
sidebar:
  label: What is Gears?
  order: 1
---

**Gears** (full name **Constructor Fabric Gears**) **is a Rust framework for building
backend platforms out of composable, secure-by-default components called _gears_.**
You write each capability once as a
self-contained gear; the runtime discovers it, wires it to its dependencies, and
runs it — in a single process on the edge, across processes over gRPC, or as
containers in Kubernetes — from the same codebase.

If you are building a multi-tenant product backend and you care about security,
authorization, and long-term structure as much as about shipping features, Gears
gives you those guarantees as part of the framework rather than as conventions you
have to re-implement and police in every service.

## What is a gear?

A **gear** is a vertically-sliced, self-contained capability. Each gear:

- **Owns its public API** through an SDK crate (`<name>-sdk`) — transport-agnostic
  traits, models, and errors. Consumers depend on the SDK, never on internals.
- **Owns its data** behind a secure ORM layer. A gear cannot touch a raw database
  connection; all access flows through `SecureConn` + `AccessScope`.
- **Is discovered at link time** and initialized in dependency order by the runtime —
  there is no central switchboard to edit when you add a gear.
- **Composes with other gears** through the typed `ClientHub` (in-process) or gRPC
  (out-of-process), behind the same SDK trait.
- **Is extensible** through plugins and the Global Type System (GTS).

## What you get out of the box

Gears ships a substantial substrate so you build features, not plumbing:

- **REST + OpenAPI** — type-safe route registration that generates the OpenAPI spec.
- **Authentication & authorization** — token validation at the edge, a PDP/PEP
  authorization model, and a `SecurityContext` propagated explicitly through the system.
- **Secure ORM** — row-level multi-tenant isolation applied automatically as SQL
  `WHERE` clauses; there is no unscoped shortcut to misuse.
- **Canonical error model** — 16 gRPC-aligned categories rendered as RFC-9457 problems.
- **Multi-tenancy** — a single-root tenant tree with barriers and resource groups.
- **OData querying** — `$filter` / `$orderby` / `$select` with cursor pagination.
- **Observability** — OpenTelemetry tracing, request IDs, and health endpoints.
- **Out-of-process gears** over gRPC, selected by configuration — no code changes.
- **FIPS 140-3-ready** crypto on Linux, macOS, and Windows.
- **`cargo gears` CLI** — a manifest-driven command-line tool for scaffolding
  workspaces, generating runnable servers, managing runtime config, building,
  deploying, and linting. See the [CLI documentation](/cli/).

See [Components & features](../reference/) for the full catalog of toolkit libraries
and ready-made system gears.

## One codebase, three deployment shapes

The same gear code compiles into three deployment shapes; you choose with configuration,
not by rewriting code:

- **Single-node** — every gear in one process (edge, on-prem, development). Gears talk
  in-process through `ClientHub`.
- **Multi-node** — gears split across processes/machines over gRPC, without container
  orchestration.
- **Kubernetes** — gears as containerized services with cluster-native discovery.

## What Gears is _not_

Gears deliberately prioritizes explicit structure, security, and long-term evolvability
over quick-start minimalism. From the framework's own non-goals:

- It is **not a no-config micro-framework** — it favors explicit structure over magic.
- It is **not a replacement for cloud infrastructure or a PaaS layer.**
- It does **not aim to ship a comprehensive set of ready-made, end-user SaaS services** —
  it gives you the platform to build them.

## The golden path

A typical path from reading to a running application:

1. [Install the toolchain and run the example server](../get-started/).
2. [Build your first gear](../get-started/your-first-gear/) — an SDK, a domain service,
   a REST endpoint, wired into the runtime.
3. Learn the [core concepts](../concepts/) — gears, the SDK pattern, `ClientHub`, the
   security model.
4. Reach for [ready-made components & features](../reference/) as you need them.
5. Understand [how the system is organized](../architecture/) when you scale up.
