---
title: Overview
description: What ships with Gears — the toolkit substrate, core gears, reusable service gears, and the dependency rules that hold them together.
sidebar:
  label: Overview
  order: 1
---

This section is the catalog of **what exists in Gears today**: the toolkit libraries every
gear builds on, the ready-made core gears that form the control plane, reusable service
gears, the cross-cutting features you reach for while building, and the current
implementation status.

:::note
Status reflects the framework source repository. `✓` means implemented; _planned_ means
designed and documented but not yet in the source tree. See
[Status and roadmap](./status-and-roadmap/).
:::

## The three-tier hierarchy

Gears are organized into three tiers with a one-way dependency direction:

```text
Service gears   (gears/)          business capabilities
      │  depend on
      ▼
Core gears      (gears/system/)   control plane: gateway, authn/authz, tenancy, …
      │  depend on
      ▼
Toolkit         (libs/)           runtime substrate: REST, DB, security, observability
```

- **[Toolkit](./toolkit/)** (`libs/`) — the low-level substrate: API middleware, DB access,
  error definitions, transport, security primitives, observability, and macros.
- **[Core gears](./core-gears/)** (`gears/system/`) — the control plane (API gateway,
  authn/authz resolvers, tenant resolver, type registry, …). They are ordinary gears, so they
  can be replaced.
- **[Reusable service gears](./service-gears/)** (`gears/`) — business capabilities shipped as
  working examples, built on the platform.

All gears under `gears/system/`, including their plugins, are **core gears**. The directory
may be renamed to `gears/core/` in the future. The **system gear group** contains gears declaring
the `system` capability; all other gears belong to the application gear group. See
[Runtime and lifecycle](../concepts/runtime-and-lifecycle/#the-system-capability).

## Gear categories

A gear encapsulates a well-defined capability and exposes versioned contracts through SDK
traits, REST APIs, gRPC, or plugin interfaces. The catalog is organized around these
categories:

- **API ingress** — the API Gateway is the public entry point for external clients.
- **Core gears** — control-plane capabilities such as authentication, authorization,
  tenancy, type registration, node discovery, and runtime orchestration.
- **Service gears** — product-facing or example capabilities built on ToolKit and core gears.
- **GenAI gears** — AI building blocks: chat, model catalogs, prompt assets, agents, web
  search, MCP tools, LLM access, memory (mostly _planned_).
- **Serverless gears** — function/workflow execution, runtime management, durable state,
  settings, and coordination primitives (_planned_).
- **Core functionality gears** — operational services such as file parsing, notifications,
  approvals, jobs, analytics, usage, events, storage, quotas, and audit.
- **Core platform integration gears** — adapters for external or enterprise systems such as
  identity providers, policy managers, license systems, and credentials stores.

## Dependency rules

These keep the component model stable:

- **External traffic enters through the API Gateway**; secure ORM access is scoped by
  `SecurityContext`.
- **Business and service gears depend on SDK contracts**, not implementation internals.
- **GenAI and serverless gears reuse lower-level platform capabilities** such as jobs,
  settings, events, storage, and usage.
- **Only integration/adapters talk to external platform services**; feature gears stay
  provider-agnostic.
- **No sideways coupling or circular dependencies**; cross-category communication goes
  through contracts.

## In this section

- **[Toolkit (libs/)](./toolkit/)** — the crates every gear builds on.
- **[Core gears](./core-gears/)** — the control plane.
- **[Reusable service gears](./service-gears/)** — working business gears.
- **[Cross-cutting features](./cross-cutting-features/)** — what you reach for while building.
- **[Status and roadmap](./status-and-roadmap/)** — implemented vs planned.
