---
status: accepted
date: 2026-02-09
decision-makers: OAGW Team
---

# Request Routing — Path-Based Routing Between API Handler, Control Plane, and Data Plane


<!-- toc -->

- [Context and Problem Statement](#context-and-problem-statement)
- [Decision Drivers](#decision-drivers)
- [Considered Options](#considered-options)
- [Decision Outcome](#decision-outcome)
  - [Routing Rules](#routing-rules)
  - [Request Flows](#request-flows)
  - [Consequences](#consequences)
  - [Confirmation](#confirmation)
- [Pros and Cons of the Options](#pros-and-cons-of-the-options)
  - [Path-based routing](#path-based-routing)
  - [Control Plane handles everything](#control-plane-handles-everything)
- [Appendix A: Proxy API Examples and Routing Behavior](#appendix-a-proxy-api-examples-and-routing-behavior)
  - [Request Classification (HTTP vs gRPC)](#request-classification-http-vs-grpc)
  - [API Call Examples](#api-call-examples)
  - [X-OAGW-Target-Host Behavior Matrix](#x-oagw-target-host-behavior-matrix)
  - [Plugin Deletion Behavior](#plugin-deletion-behavior)
  - [Audit Log JSON Format](#audit-log-json-format)
- [More Information](#more-information)
- [Traceability](#traceability)

<!-- /toc -->

**ID**: `cpt-cf-oagw-adr-request-routing`

## Context and Problem Statement

With three logical components (API Handler, Data Plane, Control Plane), OAGW needs to define how inbound requests are routed between them. Management operations (CRUD for upstreams/routes/plugins) modify configuration, while proxy operations execute calls to external services. The question is: which component handles which operations?

## Decision Drivers

* Clear separation of management vs proxy concerns
* Minimal latency for management operations (no unnecessary hops)
* Control Plane focus on config data ownership, database access, and cache invalidation
* Data Plane focus on proxy orchestration and plugin execution
* Simple, deterministic routing rules

## Considered Options

* Path-based routing: API Handler routes based on URL path
* Control Plane handles everything (all requests go to CP)

## Decision Outcome

Chosen option: "Path-based routing", because it provides the shortest path for each operation type and maintains clear separation of concerns.

### Routing Rules

| Path Pattern | Routed To | Purpose |
|---|---|---|
| `/api/oagw/v1/upstreams/*` | Control Plane | Upstream CRUD |
| `/api/oagw/v1/routes/*` | Control Plane | Route CRUD |
| `/api/oagw/v1/plugins/*` | Control Plane | Plugin CRUD |
| `/api/oagw/v1/proxy/*` | Data Plane | Proxy requests |

### Request Flows

**Management Operations** (e.g., `POST /upstreams`):

```text
Client → API Handler (auth, rate limit) → Control Plane (validate, write DB, invalidate cache) → Response
```

**Proxy Operations** (e.g., `GET /proxy/openai/v1/chat/completions`):

```text
Client → API Handler (auth, rate limit) → Data Plane (orchestrate)
  → Control Plane (resolve upstream config)
  → Control Plane (resolve route config)
  → DP: Execute plugins (auth, guard, transform)
  → DP: HTTP call to external service
→ Response
```

### Consequences

* Good, because clear separation: CP = data management, DP = request execution
* Good, because shorter path for management operations (API → CP direct)
* Good, because Data Plane remains focused on proxy logic
* Good, because Control Plane can optimize cache invalidation during writes
* Bad, because Data Plane depends on Control Plane for every proxy request (cache misses)
* Neutral, mitigated by Data Plane L1 cache for hot configs

### Confirmation

Verified by inspecting REST handler registration: management endpoints route to `ControlPlaneService` trait methods, proxy endpoints route to `DataPlaneService` trait methods.

## Pros and Cons of the Options

### Path-based routing

API Handler routes requests based on URL path prefix.

* Good, because deterministic routing with no ambiguity
* Good, because management operations take shortest path to data owner
* Good, because proxy operations go directly to orchestrator
* Good, because config resolution is separated from request orchestration (separation of concerns)
* Bad, because routing logic is hardcoded in API Handler

### Control Plane handles everything

All requests go to CP, which calls DP as needed.

* Good, because single entry point simplifies routing
* Bad, because management operations don't need CP's orchestration logic
* Bad, because adds unnecessary hop for config CRUD
* Bad, because CP becomes bottleneck for all operations

## Appendix A: Proxy API Examples and Routing Behavior

### Request Classification (HTTP vs gRPC)

At request time, the proxy handler resolves `{alias}` to an upstream first, then uses the upstream's `protocol` to determine which route match keys to apply:

- If `upstream.protocol` is HTTP, match routes using HTTP match keys (method allowlist + longest path prefix).
- If `upstream.protocol` is gRPC, match routes using gRPC match keys (`(service, method)` parsed from the gRPC request path `/{service}/{method}`).

`Content-Type` may be validated as an additional safety check, but is not required for selecting the match strategy.

In the persistence model this corresponds to `oagw_route.match_type` selecting the match key table (`oagw_route_http_match` vs `oagw_route_grpc_match`) as defined in [ADR: Storage Schema](./0009-storage-schema.md).

### API Call Examples

**Example 1: Single-Endpoint Upstream (no header required)**

```http
POST /api/oagw/v1/proxy/api.openai.com/v1/chat/completions HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token>
Content-Type: application/json

{
  "model": "gpt-4",
  "messages": [{"role": "user", "content": "Hello"}]
}
```

Upstream configuration: Single endpoint `api.openai.com:443`. The `X-OAGW-Target-Host` header is optional (ignored if provided).

**Example 2: Multi-Endpoint with Explicit Alias and X-OAGW-Target-Host**

```http
GET /api/oagw/v1/proxy/my-service/v1/status HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token>
X-OAGW-Target-Host: server-a.example.com
```

Upstream configuration: Explicit alias `my-service` with endpoints `server-a.example.com` and `server-b.example.com`. The `X-OAGW-Target-Host` header routes to a specific endpoint, bypassing round-robin load balancing.

**Example 3: Multi-Endpoint with Common Suffix Alias (header required)**

```http
POST /api/oagw/v1/proxy/vendor.com/v1/api/resource HTTP/1.1
Host: oagw.example.com
Authorization: Bearer <token>
X-OAGW-Target-Host: us.vendor.com
Content-Type: application/json

{"key": "value"}
```

Upstream configuration: Common suffix alias `vendor.com` with endpoints `us.vendor.com` and `eu.vendor.com`. The `X-OAGW-Target-Host` header is **required** to disambiguate the target endpoint.

### X-OAGW-Target-Host Behavior Matrix

| Scenario | Endpoints | Alias Type | Header Present | Behavior |
|---|---|---|---|---|
| Single endpoint | 1 | Any | No | Route to endpoint (no change) |
| Single endpoint | 1 | Any | Yes | Validate and route (header optional but validated if present) |
| Multi-endpoint | 2+ | Explicit (no common suffix) | No | Round-robin load balancing |
| Multi-endpoint | 2+ | Explicit (no common suffix) | Yes | Route to specific endpoint (bypass load balancing) |
| Multi-endpoint | 2+ | Common suffix | No | 400 Bad Request (missing required header) |
| Multi-endpoint | 2+ | Common suffix | Yes | Route to specific endpoint |

### Plugin Deletion Behavior

Plugin deletion fails with `409 Conflict` when the plugin is referenced by any upstream or route:

```http
DELETE /api/oagw/v1/plugins/gts.x.core.oagw.guard_plugin.v1~550e8400-e29b-41d4-a716-446655440000
```

**Success** (plugin not in use):

```http
HTTP/1.1 204 No Content
```

**Failure** (plugin in use):

```http
HTTP/1.1 409 Conflict
Content-Type: application/problem+json

{
  "type": "gts.x.core.errors.err.v1~x.oagw.plugin.in_use.v1",
  "title": "Plugin In Use",
  "status": 409,
  "detail": "Plugin is referenced by 3 upstream(s) and 2 route(s)",
  "plugin_id": "gts.x.core.oagw.guard_plugin.v1~550e8400-e29b-41d4-a716-446655440000",
  "referenced_by": {
    "upstreams": ["gts.x.core.oagw.upstream.v1~..."],
    "routes": ["gts.x.core.oagw.route.v1~..."]
  }
}
```

### Audit Log JSON Format

Structured JSON logs sent to stdout, ingested by centralized logging system (e.g., ELK, Loki):

```json
{
  "timestamp": "2026-02-03T11:09:37.431Z",
  "level": "INFO",
  "event": "proxy_request",
  "request_id": "req_abc123",
  "tenant_id": "tenant_xyz",
  "principal_id": "user_456",
  "host": "api.openai.com",
  "path": "/v1/chat/completions",
  "method": "POST",
  "status": 200,
  "duration_ms": 245,
  "request_size": 512,
  "response_size": 2048,
  "error_type": null
}
```

## More Information

- [ADR: Component Architecture](./0001-component-architecture.md) — Defines CP/DP trait separation
- [ADR: Control Plane Caching](./0007-data-plane-caching.md) — L1 cache mitigates DP→CP dependency
- [ADR: State Management](./0008-state-management.md) — Cache and rate limiter ownership

## Traceability

- **PRD**: [PRD.md](../PRD.md)
- **DESIGN**: [DESIGN.md](../DESIGN.md)

This decision directly addresses the following requirements or design elements:

* `cpt-cf-oagw-fr-upstream-mgmt` — Management operations routed to Control Plane
* `cpt-cf-oagw-fr-route-mgmt` — Management operations routed to Control Plane
* `cpt-cf-oagw-fr-request-proxy` — Proxy operations routed to Data Plane
* `cpt-cf-oagw-interface-management-api` — Management API endpoint routing
* `cpt-cf-oagw-interface-proxy-api` — Proxy API endpoint routing
