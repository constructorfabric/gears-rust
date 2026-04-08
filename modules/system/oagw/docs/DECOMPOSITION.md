# Decomposition: Outbound API Gateway (OAGW)


<!-- toc -->

- [1. Overview](#1-overview)
- [2. Entries](#2-entries)
  - [1. Core Domain & Storage Foundation - HIGH](#1-core-domain--storage-foundation---high)
  - [2. Upstream & Route Management - HIGH](#2-upstream--route-management---high)
  - [3. Plugin System - HIGH](#3-plugin-system---high)
  - [4. HTTP Proxy Engine - HIGH](#4-http-proxy-engine---high)
  - [5. Multi-Tenant Configuration Hierarchy - MEDIUM](#5-multi-tenant-configuration-hierarchy---medium)
  - [6. Rate Limiting & Resilience - MEDIUM](#6-rate-limiting--resilience---medium)
  - [7. Streaming & Protocol Support - MEDIUM](#7-streaming--protocol-support---medium)
  - [8. Observability & Security Hardening - MEDIUM](#8-observability--security-hardening---medium)
- [3. Feature Dependencies](#3-feature-dependencies)

<!-- /toc -->

**Overall implementation status:**
- [ ] `p1` - **ID**: `cpt-cf-oagw-status-overall`

## 1. Overview

The OAGW design is decomposed into eight features organized along functional boundaries with high cohesion and minimal coupling. The decomposition follows the Control Plane / Data Plane separation established in the DESIGN, with a shared foundation layer providing domain model, storage, SDK, and module wiring.

**Decomposition strategy**:

- **Foundation first**: Domain entities, database schema, SDK crate, and ModKit wiring form the base layer that all other features depend on.
- **Vertical slices by concern**: Management API (Control Plane CRUD), Plugin System, and Proxy Engine (Data Plane) are independent features that can be developed in parallel once the foundation is in place.
- **Cross-cutting concerns deferred**: Multi-tenant hierarchy, rate limiting/resilience, streaming, and observability/security build on top of the proxy engine and can be developed incrementally.

**Priority allocation**: Four HIGH-priority features (p1) establish the core proxy pipeline; four MEDIUM-priority features (p2) add production-grade capabilities.

**Shared design element rationale**: Several broad DESIGN IDs span multiple functional areas and are intentionally referenced by more than one feature. Each feature owns a distinct sub-concern:

- `cpt-cf-oagw-component-model` — Feature 2 owns CP CRUD, Feature 3 owns plugin subsystem, Feature 4 owns DP proxy, Feature 5 owns hierarchy/sharing, Feature 6 owns rate limiting/circuit breaker, Feature 8 owns CORS/security.
- `cpt-cf-oagw-interface-api` — Feature 2 owns management endpoints, Feature 4 owns proxy endpoint, Feature 7 owns streaming variants.
- `cpt-cf-oagw-db-schema` — Feature 1 owns schema creation and migrations; Features 2–6 reference it as a data dependency for their domain operations.
- `cpt-cf-oagw-fr-error-codes` — Feature 2 owns management error codes, Feature 4 owns proxy error codes.
- `cpt-cf-oagw-nfr-ssrf-protection` — Feature 4 owns request-path SSRF guards, Feature 8 owns IP pinning and header-stripping hardening.
- `cpt-cf-oagw-nfr-credential-isolation` — Feature 3 owns auth plugin credential handling, Feature 4 owns proxy-time credential flow.

## 2. Entries

### 1. [Core Domain & Storage Foundation](features/0001-cpt-cf-oagw-feature-domain-foundation.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-oagw-feature-domain-foundation`

- **Purpose**: Establish domain model entities, database schema, ModKit module wiring, SDK crate, and GTS type provisioning — the shared foundation all other features depend on.

- **Depends On**: None

- **Scope**:
  - Domain entities: Upstream, Route, Plugin, ServerConfig, Endpoint
  - All `oagw_*` database tables and migrations
  - ModKit module wiring (`module.rs`, `config.rs`, `OagwConfig`)
  - SDK crate (`oagw-sdk`): `ServiceGatewayClientV1` trait, SDK models, `ServiceGatewayError`
  - GTS type provisioning (`type_provisioning.rs`): schema and instance registration
  - DDD-Light layering setup (`domain/`, `infra/`, `api/rest/`)

- **Out of scope**:
  - CRUD handler implementations (Feature 2)
  - Plugin trait implementations (Feature 3)
  - Proxy execution logic (Feature 4)

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-oagw-nfr-multi-tenancy`

- **Design Principles Covered**:
  - `cpt-cf-oagw-principle-tenant-scope`

- **Design Constraints Covered**:
  - `cpt-cf-oagw-constraint-modkit-deploy`
  - `cpt-cf-oagw-constraint-multi-sql`

- **Domain Model Entities**:
  - Upstream
  - Route
  - Plugin
  - ServerConfig
  - Endpoint

- **Design Components**:
  - `cpt-cf-oagw-design-domain-model`
  - `cpt-cf-oagw-db-schema`
  - `cpt-cf-oagw-design-layers`
  - `cpt-cf-oagw-design-dependencies`
  - `cpt-cf-oagw-design-drivers`
  - `cpt-cf-oagw-design-overview`

- **API**:
  - None (foundation layer; no REST endpoints)

- **Sequences**:
  - None

- **Data**:
  - `cpt-cf-oagw-db-schema`

- **Phases**: Single-phase implementation — no sub-decomposition required.

---

### 2. [Upstream & Route Management](features/0002-cpt-cf-oagw-feature-management-api.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-oagw-feature-management-api`

- **Purpose**: Implement Control Plane CRUD operations for upstreams and routes with REST API handlers, validation, enable/disable semantics, alias enforcement, and OData query support.

- **Depends On**: `cpt-cf-oagw-feature-domain-foundation`

- **Scope**:
  - `ControlPlaneService` CRUD operations for upstreams and routes
  - REST handlers for `/api/oagw/v1/upstreams/*` and `/api/oagw/v1/routes/*`
  - DTOs with serde and utoipa annotations
  - Alias enforcement (auto-derived for hostnames, explicit for IPs) and `(tenant_id, alias)` uniqueness enforcement
  - Enable/disable semantics with ancestor inheritance
  - OData query support (`$filter`, `$select`, `$orderby`, `$top`, `$skip`)
  - RFC 9457 Problem Details error responses
  - GTS anonymous identifiers in API path parameters

- **Out of scope**:
  - Plugin CRUD (Feature 3)
  - Proxy endpoint (Feature 4)
  - Hierarchical configuration merge (Feature 5)

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-oagw-fr-upstream-mgmt`
  - [ ] `p1` - `cpt-cf-oagw-fr-route-mgmt`
  - [ ] `p1` - `cpt-cf-oagw-fr-enable-disable`
  - [ ] `p1` - `cpt-cf-oagw-fr-error-codes`

- **Design Principles Covered**:
  - `cpt-cf-oagw-principle-tenant-scope`
  - `cpt-cf-oagw-principle-rfc9457`

- **Design Constraints Covered**:
  - None (inherits from Feature 1)

- **Domain Model Entities**:
  - Upstream
  - Route
  - ServerConfig
  - Endpoint

- **Design Components**:
  - `cpt-cf-oagw-component-model`
  - `cpt-cf-oagw-interface-api`

- **API**:
  - POST /api/oagw/v1/upstreams
  - GET /api/oagw/v1/upstreams
  - GET /api/oagw/v1/upstreams/{id}
  - PUT /api/oagw/v1/upstreams/{id}
  - DELETE /api/oagw/v1/upstreams/{id}
  - POST /api/oagw/v1/routes
  - GET /api/oagw/v1/routes
  - GET /api/oagw/v1/routes/{id}
  - PUT /api/oagw/v1/routes/{id}
  - DELETE /api/oagw/v1/routes/{id}

- **Sequences**:
  - None

- **Data**:
  - `cpt-cf-oagw-db-schema`

- **Phases**: Single-phase implementation — no sub-decomposition required.

---

### 3. [Plugin System](features/0003-cpt-cf-oagw-feature-plugin-system.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-oagw-feature-plugin-system`

- **Purpose**: Implement the three-type plugin model (Auth/Guard/Transform) with built-in plugins, plugin registry, GTS identification model, custom Starlark sandbox, lifecycle management, and garbage collection.

- **Depends On**: `cpt-cf-oagw-feature-domain-foundation`

- **Scope**:
  - `AuthPlugin`, `GuardPlugin`, `TransformPlugin` trait definitions
  - All 11 built-in plugin implementations (6 auth, 2 guard, 3 transform)
  - Plugin identification model: named plugins (GTS identifiers) and UUID-backed custom plugins
  - In-process plugin registry for named plugins
  - REST handlers for `/api/oagw/v1/plugins/*` (create, list, get, delete, get source)
  - Starlark sandbox execution (no network/file I/O, timeout/memory limits)
  - Plugin immutability (no updates; version by creating new plugin)
  - Plugin GC: `gc_eligible_at` marking when unlinked, periodic cleanup
  - Plugin binding storage (`oagw_upstream_plugin`, `oagw_route_plugin`)
  - Auth plugin credential resolution via `cred_store` secret references

- **Out of scope**:
  - Plugin chain execution order (Feature 4)
  - Starlark standard library extensions (future work)

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-oagw-fr-plugin-system`
  - [ ] `p1` - `cpt-cf-oagw-fr-builtin-plugins`
  - [ ] `p1` - `cpt-cf-oagw-fr-auth-injection`
  - [ ] `p1` - `cpt-cf-oagw-nfr-credential-isolation`
  - [ ] `p3` - `cpt-cf-oagw-nfr-starlark-sandbox`

- **Design Principles Covered**:
  - `cpt-cf-oagw-principle-plugin-immutable`
  - `cpt-cf-oagw-principle-cred-isolation`

- **Design Constraints Covered**:
  - None

- **Domain Model Entities**:
  - Plugin

- **Design Components**:
  - `cpt-cf-oagw-component-model`

- **API**:
  - POST /api/oagw/v1/plugins
  - GET /api/oagw/v1/plugins
  - GET /api/oagw/v1/plugins/{id}
  - DELETE /api/oagw/v1/plugins/{id}
  - GET /api/oagw/v1/plugins/{id}/source

- **Sequences**:
  - None

- **Data**:
  - `cpt-cf-oagw-db-schema`

- **Phases**: Single-phase implementation — no sub-decomposition required.

---

### 4. [HTTP Proxy Engine](features/0004-cpt-cf-oagw-feature-proxy-engine.md) - HIGH

- [ ] `p1` - **ID**: `cpt-cf-oagw-feature-proxy-engine`

- **Purpose**: Implement the Data Plane proxy execution flow: resolve upstream by alias, match route, execute plugin chain (Auth→Guard→Transform), forward HTTP request via Pingora in-memory bridge, transform response, and return with error source distinction.

- **Depends On**: `cpt-cf-oagw-feature-management-api`, `cpt-cf-oagw-feature-plugin-system`

- **Scope**:
  - `DataPlaneService` implementation (`infra/proxy/service.rs`)
  - Proxy request flow: alias resolution → route matching → config merge → plugin chain → HTTP forward
  - Plugin chain execution order: Auth → Guards → Transform(on_request) → upstream call → Transform(on_response/on_error)
  - Upstream-before-route plugin composition (`[U1, U2] + [R1, R2]`)
  - Header transformation: set/add/remove operations, hop-by-hop stripping, passthrough control
  - Body validation: Content-Length, max size (100MB), Transfer-Encoding
  - Guard rules: method allowlist, query allowlist, path suffix validation
  - Error source distinction: `X-OAGW-Error-Source: gateway|upstream` header
  - RFC 9457 Problem Details for all gateway errors
  - Pingora proxy engine with connection pooling and load balancing
  - Proxy endpoint: `{METHOD} /api/oagw/v1/proxy/{alias}[/{path}][?{query}]`

- **Out of scope**:
  - Multi-tenant hierarchy merge (Feature 5)
  - Rate limiting enforcement (Feature 6)
  - SSE/WebSocket/WebTransport streaming (Feature 7)
  - Metrics and audit logging (Feature 8)

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-oagw-fr-request-proxy`
  - [ ] `p1` - `cpt-cf-oagw-fr-header-transform`
  - [ ] `p1` - `cpt-cf-oagw-fr-error-codes`
  - [ ] `p1` - `cpt-cf-oagw-nfr-low-latency`
  - [ ] `p1` - `cpt-cf-oagw-nfr-input-validation`
  - [ ] `p1` - `cpt-cf-oagw-nfr-ssrf-protection`
  - [ ] `p1` - `cpt-cf-oagw-nfr-credential-isolation`

- **Design Principles Covered**:
  - `cpt-cf-oagw-principle-no-retry`
  - `cpt-cf-oagw-principle-no-cache`
  - `cpt-cf-oagw-principle-error-source`
  - `cpt-cf-oagw-principle-rfc9457`
  - `cpt-cf-oagw-principle-cred-isolation`

- **Design Constraints Covered**:
  - `cpt-cf-oagw-constraint-body-limit`
  - `cpt-cf-oagw-constraint-https-only`

- **Domain Model Entities**:
  - Upstream
  - Route
  - Plugin
  - ServerConfig
  - Endpoint

- **Design Components**:
  - `cpt-cf-oagw-component-model`
  - `cpt-cf-oagw-interface-api`

- **API**:
  - {METHOD} /api/oagw/v1/proxy/{alias}/{path}

- **Sequences**:
  - `cpt-cf-oagw-seq-proxy-flow`

- **Data**:
  - `cpt-cf-oagw-db-schema`

- **Phases**: Single-phase implementation — no sub-decomposition required.

---

### 5. [Multi-Tenant Configuration Hierarchy](features/0005-cpt-cf-oagw-feature-tenant-hierarchy.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-oagw-feature-tenant-hierarchy`

- **Purpose**: Implement hierarchical configuration override across tenant tree with sharing modes, alias shadowing, merge strategies, and permission-based override control.

- **Depends On**: `cpt-cf-oagw-feature-management-api`

- **Scope**:
  - Sharing modes: `private` (owner only), `inherit` (descendant can override), `enforce` (descendant cannot override)
  - Configuration layering: upstream (base) < route < tenant (highest priority)
  - Hierarchical merge strategies per field: auth override, rate limit `min()`, plugin concatenation, tag union (add-only), CORS union
  - Alias shadowing resolution: walk tenant hierarchy from descendant to root, closest match wins
  - Enforced ancestor constraints applied across shadowing
  - Permission model: `oagw:upstream:bind`, `oagw:upstream:override_auth`, `oagw:upstream:override_rate`, `oagw:upstream:add_plugins`
  - Secret access control: `cred_store` checks tenant visibility for `secret_ref`
  - Upstream binding flow for inherited upstreams with tenant-local tag additions

- **Out of scope**:
  - `cred_store` internals (external dependency)
  - Tenant hierarchy resolution (platform responsibility)

- **Requirements Covered**:
  - [ ] `p2` - `cpt-cf-oagw-fr-config-layering`
  - [ ] `p2` - `cpt-cf-oagw-fr-hierarchical-config`
  - [ ] `p2` - `cpt-cf-oagw-fr-alias-resolution`

- **Design Principles Covered**:
  - `cpt-cf-oagw-principle-tenant-scope`

- **Design Constraints Covered**:
  - `cpt-cf-oagw-constraint-no-direct-internet`

- **Domain Model Entities**:
  - Upstream (sharing fields)
  - Route (sharing fields)

- **Design Components**:
  - `cpt-cf-oagw-component-model`

- **API**:
  - POST /api/oagw/v1/upstreams (sharing fields in request body)
  - PUT /api/oagw/v1/upstreams/{id} (sharing fields in request body)

- **Sequences**:
  - None

- **Data**:
  - `cpt-cf-oagw-db-schema`

- **Phases**: Single-phase implementation — no sub-decomposition required.

---

### 6. [Rate Limiting & Resilience](features/0006-cpt-cf-oagw-feature-rate-limiting.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-oagw-feature-rate-limiting`

- **Purpose**: Implement rate limiting at upstream and route levels with token bucket algorithm, configurable strategies, circuit breaker state machine, concurrency control, and backpressure queueing.

- **Depends On**: `cpt-cf-oagw-feature-proxy-engine`

- **Scope**:
  - Token bucket rate limiter with configurable rate, window, and capacity
  - Rate limit cost per request
  - Rate limit scopes: global, tenant, user, IP
  - Strategies: reject (429 + `Retry-After`), queue (bounded capacity), degrade
  - Hierarchical rate limit merge: `effective = min(ancestor.enforced, descendant)`
  - Circuit breaker state machine: CLOSED → OPEN → HALF_OPEN → CLOSED
  - Circuit breaker configuration: failure threshold, recovery timeout, half-open probe count
  - Concurrency control: per-scope in-flight request limits
  - Backpressure queueing: bounded queue with graceful degradation
  - Distributed state via Redis (optional) for rate limits and circuit breaker

- **Out of scope**:
  - Custom rate limit algorithms beyond token bucket
  - Dynamic rate limit adjustment via API (future work)

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-oagw-fr-rate-limiting`
  - [ ] `p1` - `cpt-cf-oagw-nfr-high-availability`

- **Design Principles Covered**:
  - None

- **Design Constraints Covered**:
  - None

- **Domain Model Entities**:
  - Upstream (rate_limit field)
  - Route (rate_limit field)

- **Design Components**:
  - `cpt-cf-oagw-component-model`
  - `cpt-cf-oagw-tech-dependencies`

- **API**:
  - None (integrated into proxy flow; rate limit config via upstream/route management)

- **Sequences**:
  - None

- **Data**:
  - `cpt-cf-oagw-db-schema`

- **Phases**: Single-phase implementation — no sub-decomposition required.

---

### 7. [Streaming & Protocol Support](features/0007-cpt-cf-oagw-feature-streaming.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-oagw-feature-streaming`

- **Purpose**: Implement SSE and WebSocket streaming with proper connection lifecycle management and HTTP/2 version negotiation (ALPN).

- **Depends On**: `cpt-cf-oagw-feature-proxy-engine`

- **Scope**:
  - SSE event streaming: open, data forwarding, close, error handling
  - WebSocket session flows: upgrade, bidirectional messaging, close
  - Connection lifecycle management for all streaming protocols
  - HTTP/2 adaptive version detection with ALPN during TLS handshake
  - Protocol version caching per host/IP (1h TTL)
  - HTTP/1.1 fallback on HTTP/2 negotiation failure

- **Out of scope**:
  - HTTP/3 (QUIC) support (future work)
  - WebTransport session flows: connection setup, stream multiplexing, close (future work)
  - gRPC streaming (future work; see `cpt-cf-oagw-adr-grpc-support`)

- **Requirements Covered**:
  - [ ] `p1` - `cpt-cf-oagw-fr-streaming`

- **Design Principles Covered**:
  - None

- **Design Constraints Covered**:
  - None

- **Domain Model Entities**:
  - Upstream (protocol field)

- **Design Components**:
  - `cpt-cf-oagw-interface-api`

- **API**:
  - {METHOD} /api/oagw/v1/proxy/{alias}/{path} (streaming variant)

- **Sequences**:
  - `cpt-cf-oagw-seq-proxy-flow`

- **Data**:
  - None

- **Phases**: Single-phase implementation — no sub-decomposition required.

---

### 8. [Observability & Security Hardening](features/0008-cpt-cf-oagw-feature-observability.md) - MEDIUM

- [ ] `p2` - **ID**: `cpt-cf-oagw-feature-observability`

- **Purpose**: Implement Prometheus metrics, structured audit logging, CORS handling, SSRF protection, and multi-layer caching for operational visibility and security hardening.

- **Depends On**: `cpt-cf-oagw-feature-proxy-engine`

- **Scope**:
  - Prometheus metrics: `oagw_requests_total`, `oagw_request_duration_seconds`, `oagw_requests_in_flight`, `oagw_errors_total`, `oagw_circuit_breaker_state`, `oagw_rate_limit_exceeded_total`, `oagw_circuit_breaker_transitions_total`, `oagw_rate_limit_usage_ratio`, `oagw_routing_target_host_used`, `oagw_routing_endpoint_selected`, `oagw_upstream_available`, `oagw_upstream_connections`
  - Histogram buckets: `[0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0]`
  - Cardinality management: no tenant labels, normalized paths, status class grouping
  - Structured JSON audit logging to stdout: request ID, tenant, host, path, method, status, duration, sizes
  - Log level policies: INFO (success), WARN (rate limit/circuit breaker), ERROR (failures/timeouts); plugin execution is traced via structured events and metrics, not log levels
  - No PII, no secrets in logs; high-frequency sampling for volume control
  - CORS built-in handler: per-upstream/route configuration, preflight OPTIONS (no upstream round-trip)
  - SSRF protection: IP pinning rules, scheme allowlist (HTTPS-only), header stripping
  - HTTP smuggling prevention: strict header parsing, CR/LF rejection, CL/TE validation
  - Multi-layer caching: DP L1 (1,000 entries, <1μs), CP L1 (10,000 entries, <1μs), CP L2 Redis (optional, ~1-2ms), DB (source of truth)

- **Out of scope**:
  - TLS certificate pinning (future work)
  - mTLS support (future work)
  - Centralized logging system deployment (infrastructure concern)

- **Requirements Covered**:
  - [ ] `p2` - `cpt-cf-oagw-nfr-observability`
  - [ ] `p1` - `cpt-cf-oagw-nfr-ssrf-protection`

- **Design Principles Covered**:
  - None

- **Design Constraints Covered**:
  - None

- **Domain Model Entities**:
  - Upstream (cors field)
  - Route (cors field)

- **Design Components**:
  - `cpt-cf-oagw-component-model`
  - `cpt-cf-oagw-tech-dependencies`

- **API**:
  - None (integrated into proxy flow and admin `/metrics` endpoint)

- **Sequences**:
  - None

- **Data**:
  - None

- **Phases**: Single-phase implementation — no sub-decomposition required.

---

## 3. Feature Dependencies

```text
cpt-cf-oagw-feature-domain-foundation
    ↓
    ├─→ cpt-cf-oagw-feature-management-api
    │       ↓
    │       ├─→ cpt-cf-oagw-feature-tenant-hierarchy
    │       │
    │       └───────┐
    │               ↓
    └─→ cpt-cf-oagw-feature-plugin-system
                    ↓
            cpt-cf-oagw-feature-proxy-engine
                    ↓
                    ├─→ cpt-cf-oagw-feature-rate-limiting
                    ├─→ cpt-cf-oagw-feature-streaming
                    └─→ cpt-cf-oagw-feature-observability
```

**Dependency Rationale**:

- `cpt-cf-oagw-feature-management-api` requires `cpt-cf-oagw-feature-domain-foundation`: CRUD operations need domain entities, database tables, and SDK types to be defined
- `cpt-cf-oagw-feature-plugin-system` requires `cpt-cf-oagw-feature-domain-foundation`: Plugin model, storage tables, and GTS type registration must exist before plugin implementations
- `cpt-cf-oagw-feature-proxy-engine` requires `cpt-cf-oagw-feature-management-api`: Proxy needs upstream/route resolution and ControlPlaneService to look up configurations
- `cpt-cf-oagw-feature-proxy-engine` requires `cpt-cf-oagw-feature-plugin-system`: Proxy executes the plugin chain (Auth→Guard→Transform) during request processing
- `cpt-cf-oagw-feature-tenant-hierarchy` requires `cpt-cf-oagw-feature-management-api`: Hierarchical config extends the base CRUD operations with sharing modes and merge strategies
- `cpt-cf-oagw-feature-rate-limiting` requires `cpt-cf-oagw-feature-proxy-engine`: Rate limiting and circuit breaker are enforced during proxy request execution
- `cpt-cf-oagw-feature-streaming` requires `cpt-cf-oagw-feature-proxy-engine`: Streaming extends the base proxy flow with SSE/WebSocket/WebTransport connection handling
- `cpt-cf-oagw-feature-observability` requires `cpt-cf-oagw-feature-proxy-engine`: Metrics and logging instrument the proxy request pipeline
- `cpt-cf-oagw-feature-management-api` and `cpt-cf-oagw-feature-plugin-system` are independent of each other and can be developed in parallel
- `cpt-cf-oagw-feature-rate-limiting`, `cpt-cf-oagw-feature-streaming`, and `cpt-cf-oagw-feature-observability` are independent of each other and can be developed in parallel
- `cpt-cf-oagw-feature-tenant-hierarchy` is independent of the proxy engine features and can be developed in parallel with Features 4-8
