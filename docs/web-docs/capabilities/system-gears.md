---
title: System gears
description: The control plane — API gateway, authn/authz resolvers, tenant resolver, type registry, and runtime orchestration.
sidebar:
  label: System gears
  order: 3
---

System gears (`gears/system/`) are the control plane. Each is an ordinary gear behind an
SDK, so it can be replaced.

| Gear | What it does | Status |
| --- | --- | --- |
| **API Gateway** | Public ingress: routing, auth middleware, rate limiting, OpenAPI publication, health endpoints | ✓ |
| **Service Discovery** | Service discovery, module loading, runtime coordination | ✓ |
| **AuthN Resolver** | Token validation (JWT/OIDC); produces `SecurityContext`. Plugins: static, OIDC | ✓ |
| **AuthZ Resolver (PDP)** | Authorization decisions + row-level constraints → `AccessScope`. Plugins: static, tenant-rules | ✓ |
| **Tenant Resolver** | Tenant tree traversal, ancestor/descendant queries, barrier semantics. Plugins: static, single-tenant, resource-group | ✓ |
| **Outbound API Gateway (OAGW)** | Centralized egress: credential resolution, auth plugins, rate limiting | ✓ |
| **Types Registry** | [GTS](https://github.com/GlobalTypeSystem/gts-rust) schema storage, lookup, instance validation | ✓ |
| **Nodes Registry** | Node inventory and capability discovery | ✓ |
| **Resource Group** | Hierarchical, tenant-scoped resource grouping for access control | ✓ |
| **gRPC Hub** | Out-of-process gear orchestration: gRPC server wiring, reflection | ✓ |
| **Usage Collector** | Measure API/compute/storage usage (push model) | SDK ✓, impl _planned_ |
| **Account Management** | Tenant/user account lifecycle when Gears runs standalone | _planned_ |

## See also

- [Request path](../../concepts/request-path/) — how a request flows through these gears.
- [API Gateway and OpenAPI](../../concepts/api-gateway-openapi/) — the ingress model.
- [Security and multi-tenancy](../../concepts/security-and-tenancy/) — authn/authz/tenancy.
