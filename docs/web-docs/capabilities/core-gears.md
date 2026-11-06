---
title: Core gears
description: The control plane — API gateway, authn/authz resolvers, tenant resolver, type registry, and runtime orchestration.
sidebar:
  label: Core gears
  order: 3
---

**Core gears** are all gears under `gears/system/`, including their plugins. They provide
the platform's control-plane and shared services. Each is an ordinary gear behind an SDK,
so it can be replaced; a deployment selects the gears it needs.

The directory may be renamed to `gears/core/` in the future. Its current name does not
select a runtime startup group.

The **system gear group** contains gears declaring the `system` capability. Currently
these are Types Registry, Service Discovery, Cluster and gRPC Hub. All other core gears, including the
API Gateway, resolvers and plugins, run in the application gear group. See
[the system capability](../../concepts/runtime-and-lifecycle/#the-system-capability)
for its startup guarantees and restrictions.

| Gear | What it does | Startup group | Status |
| --- | --- | --- | --- |
| **API Gateway** | Public ingress: routing, auth middleware, rate limiting, OpenAPI publication, health endpoints | Application gear group | ✓ |
| **Service Discovery** | Service discovery, module loading, runtime coordination | System gear group | ✓ |
| **AuthN Resolver** | Token validation (JWT/OIDC); produces `SecurityContext`. Plugins: static, OIDC | Application gear group | ✓ |
| **AuthZ Resolver (PDP)** | Authorization decisions + row-level constraints → `AccessScope`. Plugins: static, tenant-rules | Application gear group | ✓ |
| **Tenant Resolver** | Tenant tree traversal, ancestor/descendant queries, barrier semantics. Plugins: static, single-tenant, resource-group | Application gear group | ✓ |
| **Outbound API Gateway (OAGW)** | Centralized egress: credential resolution, auth plugins, rate limiting | Application gear group | ✓ |
| **Types Registry** | [GTS](https://github.com/GlobalTypeSystem/gts-rust) schema storage, lookup, instance validation | System gear group | ✓ |
| **Nodes Registry** | Node inventory and capability discovery | Application gear group | ✓ |
| **Resource Group** | Hierarchical, tenant-scoped resource grouping for access control | Application gear group | ✓ |
| **gRPC Hub** | Out-of-process gear orchestration: gRPC server wiring, reflection | System gear group | ✓ |
| **Cluster** | Backend-selected coordination contracts: distributed cache, leader election and distributed locks | System gear group | ✓ |
| **Usage Collector** | Measure API/compute/storage usage (push model) | Application gear group (planned) | SDK ✓, impl _planned_ |
| **Account Management** | Tenant/user account lifecycle when Gears runs standalone | Application gear group (planned) | _planned_ |

## See also

- [Request path](../../concepts/request-path/) — how a request flows through these gears.
- [API Gateway and OpenAPI](../../concepts/api-gateway-openapi/) — the ingress model.
- [Security and multi-tenancy](../../concepts/security-and-tenancy/) — authn/authz/tenancy.
