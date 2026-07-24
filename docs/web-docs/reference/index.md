---
title: Components & features
description: What the Gears toolkit and system gears give you out of the box.
sidebar:
  label: Components & features
  order: 1
---

This is the catalog of what ships with Gears: the toolkit libraries, the ready-made system
gears, the example service gears, and the cross-cutting features you can use immediately.

:::note
Status reflects the framework source repository. `✓` means implemented; _planned_ means
designed and documented but not yet in the source tree.
:::

## Toolkit libraries (`libs/`)

The low-level substrate every gear builds on.

![Libraries dependency graph](../assets/libs-dependencies.drawio.svg)

| Crate | What it provides | Key types / macros |
| --- | --- | --- |
| `toolkit` | Core runtime: gear lifecycle (`HostRuntime`), in-process composition (`ClientHub`), REST/OpenAPI wiring, SSE, transactional outbox, telemetry | `#[toolkit::gear]`, `GearCtx`, `ClientHub`, `OperationBuilder`, `SseBroadcaster` |
| `toolkit-macros` | Proc-macros for gear discovery and domain validation | `#[domain_model]`, `#[gear(...)]` |
| `toolkit-db` | Secure ORM over SeaORM: scoped access, transactions | `SecureConn`, `SecureTx`, `DBProvider`, `AccessScope` |
| `toolkit-db-macros` | Entity security-dimension derive | `#[derive(Scopable)]`, `#[secure(...)]` |
| `toolkit-security` | Core security types | `SecurityContext`, `AccessScope`, `ScopableEntity` |
| `toolkit-canonical-errors` | 16-category canonical errors + RFC-9457 rendering | `CanonicalError`, `Problem` |
| `toolkit-auth` | AuthN/AuthZ integration types | `PolicyEnforcer`, `AuthZResolverClient` |
| `toolkit-http` | HTTP client with OpenTelemetry tracing | `HttpClient`, `.with_otel()` |
| `toolkit-odata` | OData `$filter` / `$orderby` / `$select` + cursor pagination | `Page<T>`, OData extractors |
| `toolkit-odata-macros` | OData-filterable DTO derive | `#[derive(ODataFilterable)]` |
| `toolkit-gts` / `-macros` | Global Type System: schema collection & generation | GTS schema registration |
| `toolkit-sdk` | SDK-pattern helpers and transport-agnostic contracts | facade/query helpers |
| `toolkit-transport-grpc` | gRPC transport for out-of-process gears | gRPC client/connect helpers |
| `toolkit-node-info`, `toolkit-utils` | Node/deployment info; shared utilities | — |
| `rustls-corecrypto-provider`, `rustls-fips-shim` | FIPS 140-3 crypto routing per platform | TLS provider shims |

## System gears (`gears/system/`)

The control plane. Each is an ordinary gear behind an SDK, so it can be replaced.

| Gear | What it does | Status |
| --- | --- | --- |
| **API Gateway** | Public ingress: routing, auth middleware, rate limiting, OpenAPI publication, health endpoints | ✓ |
| **Gear Orchestrator** | Service discovery, module loading, runtime coordination | ✓ |
| **AuthN Resolver** | Token validation (JWT/OIDC); produces `SecurityContext`. Plugins: static, OIDC | ✓ |
| **AuthZ Resolver (PDP)** | Authorization decisions + row-level constraints → `AccessScope`. Plugins: static, tenant-rules | ✓ |
| **Tenant Resolver** | Tenant tree traversal, ancestor/descendant queries, barrier semantics. Plugins: static, single-tenant, resource-group | ✓ |
| **Outbound API Gateway (OAGW)** | Centralized egress: credential resolution, auth plugins, rate limiting | ✓ |
| **Types Registry** | GTS schema storage, lookup, instance validation | ✓ |
| **Nodes Registry** | Node inventory and capability discovery | ✓ |
| **Resource Group** | Hierarchical, tenant-scoped resource grouping for access control | ✓ |
| **gRPC Hub** | Out-of-process gear orchestration: gRPC server wiring, reflection | ✓ |
| **Usage Collector** | Measure API/compute/storage usage (push model) | SDK ✓, impl _planned_ |
| **Account Management** | Tenant/user account lifecycle when Gears runs standalone | _planned_ |

## Service gears (`gears/`)

Business gears shipped as working examples.

| Gear | What it does | Status |
| --- | --- | --- |
| **File Parser** | Parse DOCX/PPTX/PDF/Markdown/HTML/text; extract text & metadata | ✓ |
| **Credentials Store** | Plugin-based secret storage with zeroize/redaction and tenant scoping | ✓ |
| **Mini Chat** | Minimal chat engine (conversations + messages) for examples/testing | ✓ |
| **Simple User Settings** | Per-user/tenant configuration with schema validation | ✓ |

A larger catalog of GenAI and platform service gears (Chat Engine, LLM Gateway, Models/
Prompts/Agents registries, Serverless, Events Broker, Durable Objects, Jobs Manager, and
more) is **designed but not yet implemented** — see [What's planned](#whats-planned).

## Cross-cutting features

What you reach for while building, and the entry point for each:

- **REST + OpenAPI** — declare routes with `OperationBuilder`; the OpenAPI spec and Swagger
  UI are generated automatically. Versioned paths (`/{service}/v{N}/…`) are enforced.
- **Canonical errors** — define a domain error, map it into the canonical model; the REST
  boundary renders RFC-9457 problems. 16 categories with fixed HTTP mappings.
- **AuthN / AuthZ** — `.authenticated()` on routes; in services call
  `PolicyEnforcer::access_scope_with(ctx, type, action, id)` to get an `AccessScope`.
- **Secure ORM** — query through `SecureConn`; entities use `#[derive(Scopable)]`. Empty
  scope yields `WHERE 1=0` (deny-by-default); tenant id is immutable on update.
- **Multi-tenancy** — resolve the tenant tree through `TenantResolverClient`; use the
  `in_tenant_subtree` predicate in policies; raise barriers with `self_managed`.
- **OData** — add `.with_odata_filter::<F>()`, `.with_odata_select()`,
  `.with_odata_orderby::<F>()`; paginate with cursor-based `Page<T>`.
- **Observability** — automatic trace spans, W3C trace-context propagation, request IDs,
  `/health` & `/healthz`; build outbound clients with `HttpClient::builder().with_otel()`.
- **Lifecycle & background tasks** — declare the `stateful` capability; the runtime drives
  ordered startup, a `post_init` barrier, and cancellation-aware shutdown.
- **Out-of-process gears** — same SDK trait, gRPC transport, selected by config
  (`runtime.type: local | oop`).
- **Global Type System (GTS)** — register schemas from Rust types; extend the domain model
  without changing existing gears.
- **Security baseline / FIPS** — Rust safety, strict Clippy + custom architecture lints (via `cargo gears lint`), `cargo-deny`,
  continuous fuzzing, and `--features fips` for validated crypto on Linux/macOS/Windows.

## What's planned

These are designed and documented in the framework but **not yet implemented**: Cluster
Plane (leader election, distributed locks, service discovery, distributed cache), Chat
Engine, LLM Gateway, Models / Prompts / AI Agents registries, MCP Registry, Agent Memory,
Web Search Gateway, URL Crawler, Model Scheduler, Local Search Index, Serverless Gateway &
Runtimes, Durable Objects, Events Broker, File Storage, Jobs Manager, Notifications,
Approvals, Analytics, Quota Enforcer, and Audit.

When you see these referenced elsewhere in the docs, treat them as roadmap unless this page
marks them `✓`.
