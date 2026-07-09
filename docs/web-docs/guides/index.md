---
title: Guides
description: Task-oriented guides for building with Gears — database, authorization, OData, gRPC, and observability.
sidebar:
  label: Overview
  order: 1
---

Task-oriented guides for the things you do while building a gear. Each is grounded in the
real framework source and the `users-info` / `calculator` examples; where the source is thin,
the guide says so and links to the example.

- **[Database patterns](./database/)** — secure, tenant-scoped persistence with
  `SecureConn`, `#[derive(Scopable)]`, transactions, and migrations.
- **[Authorization](./authorization/)** — the PDP/PEP flow: `PolicyEnforcer`,
  `AccessScope`, constraint predicates, and route-level auth.
- **[Pagination & filtering](./odata/)** — OData `$filter` / `$orderby` / `$select`
  and cursor-based pages.
- **[Out-of-process gears](./out-of-process/)** — run a gear as a separate gRPC
  service behind the same SDK, selected by configuration.
- **[Observability](./observability/)** — OpenTelemetry tracing, request IDs, health
  endpoints, and instrumented HTTP clients.

New here? Start with the [first-gear walkthrough](../get-started/your-first-gear/), which uses
all of these together.
