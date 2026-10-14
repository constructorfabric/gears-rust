---
title: Cross-cutting features
description: What you reach for while building — REST/OpenAPI, canonical errors, authz, secure ORM, multi-tenancy, OData, observability, lifecycle, OoP, GTS, and FIPS.
sidebar:
  label: Cross-cutting features
  order: 5
---

These are the features you reach for while building, and the entry point for each.

- **REST + OpenAPI** — declare routes with `OperationBuilder`; the OpenAPI spec and Swagger
  UI are generated automatically. Versioned paths (`/{service}/v{N}/…`) are enforced. See
  [API Gateway and OpenAPI](../../concepts/api-gateway-openapi/).
- **Canonical errors** — define a domain error, map it into the canonical model; the REST
  boundary renders RFC-9457 problems. 16 categories with fixed HTTP mappings. See
  [Error model](../../concepts/error-model/).
- **AuthN / AuthZ** — `.authenticated()` on routes; in services call
  `PolicyEnforcer::access_scope_with(ctx, type, action, id)` to get an `AccessScope`. See
  [Add authorization](../../build-with-gears/add-authorization/).
- **Secure ORM** — query through `SecureConn`; entities use `#[derive(Scopable)]`. Empty
  scope yields `WHERE 1=0` (deny-by-default); tenant id is immutable on update. See
  [Add a database](../../build-with-gears/add-a-database/).
- **Multi-tenancy** — resolve the tenant tree through `TenantResolverClient`; use the
  `in_tenant_subtree` predicate in policies; raise barriers with `self_managed`. See
  [Security and multi-tenancy](../../concepts/security-and-tenancy/).
- **OData** — add `.with_odata_filter::<F>()`, `.with_odata_select()`,
  `.with_odata_orderby::<F>()`; paginate with cursor-based `Page<T>`. See
  [Add pagination and filtering](../../build-with-gears/add-pagination-odata/).
- **Observability** — automatic trace spans, W3C trace-context propagation, request IDs,
  `/health` & `/healthz`; build outbound clients with `HttpClient::builder().with_otel()`.
  See [Add observability](../../build-with-gears/add-observability/).
- **Lifecycle & background tasks** — declare the `stateful` capability; the runtime drives
  ordered startup, a `post_init` barrier, and cancellation-aware shutdown. See
  [Runtime and lifecycle](../../concepts/runtime-and-lifecycle/).
- **Out-of-process gears** — same SDK trait, REST transport (gRPC opt-in), selected by config
  (`runtime.type: local | oop`). See
  [Run a gear out-of-process](../../build-with-gears/out-of-process/).
- **[Global Type System (GTS)](https://github.com/GlobalTypeSystem/gts-rust)** — register
  schemas from Rust types; extend the domain model without changing existing gears. See
  [Type system (GTS)](../../concepts/type-system-gts/).
- **Security baseline / FIPS** — Rust safety, strict Clippy + custom Dylints, `cargo-deny`,
  continuous fuzzing, and `--features fips` for validated crypto on Linux/macOS/Windows. See
  [Compliance and FIPS](../../concepts/compliance-and-fips/).

## Extension model

Gears use the [Global Type System (GTS)](https://github.com/GlobalTypeSystem/gts-rust) to define globally identified type definitions and instances. This lets teams add plugin contracts, event types, settings schemas, model metadata, permissions, license types, and tool definitions without changing existing gears code or endpoints.

The plugin model is open-closed: host gears define interfaces in SDK crates, plugins implement those interfaces, and the runtime discovers implementations through typed registration. Adding a new provider or integration should usually mean adding a plugin, not modifying the host gear. See [Plugins and extension points](../../concepts/plugins-and-extension-points/).
